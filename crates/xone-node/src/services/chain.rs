//! The `chain` service: a minimal live-follow driver over `xone-net` that the XB-14 pipeline replaces.
//!
//! It keeps no schedule of its own. The best peer by TD rank is asked for the headers that follow our
//! head (`GetBlockHeaders` from our head hash, so that the first answer must be that block: a peer
//! that answers from another block is put aside), then for the bodies of those headers; the
//! blocks go through `Core::import` in order. An empty continuation from the best peer is the
//! caught-up proof (hash equality: the peer's chain holds our head and nothing above it). `NewBlock`
//! that extends the head is imported at once, also while a request is in flight (geth-XDC
//! `eth/handler_eth.go:205-296`); the answer to that request then skips what is already ours. A
//! peer that has not yet got our head, or not yet imported the head it announced, is "not yet", not
//! wrong: it is set aside for [`NOT_YET`] only. Receipts are not requested: the node executes every
//! block and compares the roots with the header, so a peer's receipts add nothing to verify.
//!
//! ponytail: one request at a time, one peer at a time, no scheduling, no fork handling above what
//! `Core` supports (the parent of a block must be the head).

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use alloy_primitives::{Bytes, B256};
use tokio::{
    runtime::Handle,
    sync::{
        mpsc::{self, Receiver},
        watch, Semaphore,
    },
    task::JoinHandle,
    time::{interval, timeout, Instant},
};
use xone_consensus::verify::Body;
use xone_net::{
    eth::{BftMessage, BlockBody, GetHeaders, Message, Origin, Raw},
    peers::{Event, Peers},
    rlpx::{PeerId, Sub},
};
use xone_primitives::Header;
use xone_sync::chain::insert::Outcome;

use super::{
    core::{now, Core},
    from_wire,
    net::NetHandle,
    pool::PoolShared,
    snap::{Request, SnapServer},
    to_wire,
};
use crate::lifecycle::{Service, ServiceError};

/// Headers asked for at a time (geth `MaxHeaderFetch` is 192).
const BATCH: u64 = 128;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BAN: Duration = Duration::from_secs(60);
/// A peer that is behind us or still importing the block it announced (geth-XDC broadcasts
/// `NewBlock` before its own import) answers an empty poll. geth-XDC never counts an empty answer at
/// the tip against a peer (`eth/sync_xdc.go:305-311`); here it only moves the next poll to another
/// peer for two block intervals.
const NOT_YET: Duration = Duration::from_secs(4);

/// Body and receipts answers read from the store at once: a 128-block batch of 20,000-tx blocks is a
/// long read, so a few run side by side and the rest wait their turn.
const SERVING: usize = 4;
/// geth `maxBodiesServe` and `maxReceiptsServe` (`eth/protocols/eth/handler.go:50,56`).
const MAX_SERVE: usize = 1024;
/// geth `softResponseLimit` (`eth/protocols/eth/handler.go:37`): the answer stops once it holds this
/// many bytes, so one request never fills the 10 MiB message limit of the transport.
const SOFT_LIMIT: usize = 2 * 1024 * 1024;

/// geth `ServiceGetBlockBodiesQuery` (`eth/protocols/eth/handlers.go:260-277`): the bodies in the order
/// asked, up to [`MAX_SERVE`] and until [`SOFT_LIMIT`] bytes are held (the block that crosses it is
/// included), and the answer stops at the first hash that `get` does not hold. A peer that holds none
/// of the range, such as an anchored node below its retention floor, answers an empty list at once: the
/// client reads "not here" and asks another peer, as it does geth. Skipping a missing block instead
/// would shift every later body against its hash.
fn bodies_answer(hashes: &[B256], mut get: impl FnMut(B256) -> Option<Body>) -> Vec<BlockBody> {
    let (mut out, mut bytes) = (vec![], 0);
    for h in hashes.iter().take(MAX_SERVE) {
        if bytes >= SOFT_LIMIT {
            break;
        }
        let Some(b) = get(*h) else {
            break;
        };
        let wire = BlockBody {
            transactions: b.transactions.iter().map(to_wire).collect(),
            uncles: b.uncles,
            withdrawals: b
                .withdrawals
                .map(|w| w.into_iter().map(xone_net::eth::Raw).collect()),
        };
        bytes += alloy_rlp::Encodable::length(&wire);
        out.push(wire);
    }
    out
}

/// geth `ServiceGetReceiptsQueryXDPOS2` (`eth/protocols/eth/handlers.go:350-373`), the same rules as
/// [`bodies_answer`] for the receipts of each block.
fn receipts_answer(
    hashes: &[B256],
    mut get: impl FnMut(B256) -> Option<Vec<alloy_primitives::Bytes>>,
) -> Vec<Raw> {
    let (mut out, mut bytes) = (vec![], 0);
    for h in hashes.iter().take(MAX_SERVE) {
        if bytes >= SOFT_LIMIT {
            break;
        }
        let Some(r) = get(*h) else {
            break;
        };
        let wire = receipts_wire(&r);
        bytes += wire.0.len();
        out.push(wire);
    }
    out
}

/// What the RPC and the logs read of the follower.
#[derive(Default)]
pub struct Progress {
    pub start: AtomicU64,
    pub highest: AtomicU64,
    /// An empty continuation from the best peer was seen at the current head.
    pub confirmed: AtomicBool,
}

struct Peer {
    version: u64,
    rank: u64,
    /// The head the peer last advertised: its Status head, then its announcements.
    head: B256,
}

enum Stage {
    /// Asked from this hash: the answer must start with it.
    Headers(B256),
    Bodies(Vec<Header>),
    /// Headers from our committed block, asked of a peer that does not know our head.
    Fork,
    /// The bodies of a peer's branch that leaves our chain above the committed block.
    Branch(Vec<Header>),
}

/// Where the peer's `headers` (consecutive, from a block we hold) leave our chain: the index of the first one
/// that is not ours. `ours(n)` is our canonical hash at `n`. `Err` when they do not link, do not start on our
/// chain, or do not end above our head (`head`): only a longer branch replaces ours (geth `keepsV2Head`,
/// `core/blockchain.go:2419-2442`).
fn branch_start(
    headers: &[Header],
    ours: impl Fn(u64) -> Option<B256>,
    head: u64,
) -> Result<usize, &'static str> {
    let first = headers.first().ok_or("no headers")?;
    if ours(first.number) != Some(first.hash()) {
        return Err("the headers do not start on our chain");
    }
    let linked = headers
        .windows(2)
        .all(|w| w[1].parent_hash == w[0].hash() && w[1].number == w[0].number + 1);
    if !linked {
        return Err("headers do not link");
    }
    let at = headers
        .iter()
        .position(|h| ours(h.number) != Some(h.hash()))
        .ok_or("no branch: every header is ours")?;
    if headers.last().map_or(0, |h| h.number) <= head {
        return Err("the branch does not end above our head");
    }
    Ok(at)
}

struct Inflight {
    peer: PeerId,
    id: u64,
    stage: Stage,
    deadline: Instant,
}

/// Serves the data requests of peers (headers, bodies, receipts), apart from the follower: an import that
/// the follower awaits (seconds, for a block of 20,000 transactions) never delays an answer, and an
/// answer never delays an import.
struct Server {
    core: Arc<Core>,
    peers: Peers,
    /// Body and receipts answers in work at once ([`SERVING`]).
    serving: Arc<Semaphore>,
}

impl Server {
    /// Answers a header, body or receipts request of a peer. Headers come from the history at once.
    /// Bodies and receipts of old blocks are read from the segment files, which for 20,000-tx blocks is
    /// long: that read runs on a blocking thread of its own, and nothing of the follower is awaited, so
    /// a request waits neither for an import in progress nor for the import lock ([`Core::records`],
    /// XB-36).
    fn serve(&self, peer: &PeerId, msg: &Message) {
        let reply = match msg {
            Message::GetBlockHeaders { id, query } => {
                let h = self.core.hist.read();
                let amount = query.amount.min(192);
                let start = match query.origin {
                    Origin::Number(n) => Some(n),
                    Origin::Hash(x) => h.number_of(x),
                };
                let mut out = vec![];
                let mut at = start;
                while let (Some(n), true) = (at, u64::try_from(out.len()).is_ok_and(|l| l < amount))
                {
                    let Some(s) = h.header_at(n) else {
                        break;
                    };
                    out.push(s.into_owned());
                    let step = query.skip.saturating_add(1);
                    at = if query.reverse {
                        n.checked_sub(step)
                    } else {
                        n.checked_add(step)
                    };
                }
                Message::BlockHeaders {
                    id: *id,
                    headers: out,
                }
            }
            Message::GetBlockBodies { id, hashes } => {
                let (id, hashes) = (*id, hashes.clone());
                return self.serve_blocking(*peer, move |core| Message::BlockBodies {
                    id,
                    bodies: bodies_answer(&hashes, |h| core.body(core.number_of(h)?, h)),
                });
            }
            Message::GetReceipts { id, hashes, .. } => {
                let (id, hashes) = (*id, hashes.clone());
                return self.serve_blocking(*peer, move |core| Message::Receipts {
                    id,
                    last_incomplete: false,
                    per_block: receipts_answer(&hashes, |h| core.receipts(core.number_of(h)?, h)),
                });
            }
            _ => return,
        };
        // a full queue of the peer drops the answer; it asks again
        let _ = self.peers.send(peer, reply);
    }

    /// Builds the answer on a blocking thread, at most [`SERVING`] at a time, and sends it.
    fn serve_blocking(&self, peer: PeerId, make: impl FnOnce(&Core) -> Message + Send + 'static) {
        let (core, peers, gate) = (self.core.clone(), self.peers.clone(), self.serving.clone());
        tokio::spawn(async move {
            let Ok(_slot) = gate.acquire_owned().await else {
                return;
            };
            if let Ok(reply) = tokio::task::spawn_blocking(move || make(&core)).await {
                // a full queue of the peer drops the answer; it asks again
                let _ = peers.send(&peer, reply);
            }
        });
    }
}

struct Follower {
    core: Arc<Core>,
    peers: Peers,
    pool: Arc<PoolShared>,
    progress: Arc<Progress>,
    known: HashMap<PeerId, Peer>,
    banned: HashMap<PeerId, Instant>,
    next_id: u64,
    inflight: Option<Inflight>,
    /// Requests of snap/1 and xpre/1, to the [`SnapServer`] task.
    snap: mpsc::Sender<Request>,
    /// Votes, timeouts and SyncInfos for the producer's BFT machine (`--mine` only).
    bft: Option<Bft>,
}

/// Where the follower hands the BFT messages of its peers: the producer verifies them, then relays them.
pub type Bft = std::sync::mpsc::Sender<(PeerId, BftMessage)>;

/// One block's receipts as eth carries them: a list whose items are the consensus encodings, a
/// typed receipt wrapped in an RLP string (the same rule as a transaction, [`to_wire`]).
pub(crate) fn receipts_wire(receipts: &[alloy_primitives::Bytes]) -> Raw {
    let items: Vec<Raw> = receipts.iter().map(to_wire).collect();
    Raw(alloy_rlp::encode(&items).into())
}

pub(crate) fn to_body(b: BlockBody) -> (Body, usize) {
    let n = b.transactions.len();
    (
        Body {
            transactions: b.transactions.iter().map(from_wire).collect(),
            uncles: b.uncles,
            withdrawals: b.withdrawals.map(|w| w.into_iter().map(|r| r.0).collect()),
        },
        n,
    )
}

impl Follower {
    fn ban(&mut self, p: PeerId, why: &str) {
        self.set_aside(p, why, BAN);
    }

    fn set_aside(&mut self, p: PeerId, why: &str, d: Duration) {
        eprintln!(
            "chain: setting peer {} aside for {} s: {why}",
            short(&p),
            d.as_secs()
        );
        self.banned.insert(p, Instant::now() + d);
        if self.inflight.as_ref().is_some_and(|i| i.peer == p) {
            self.inflight = None;
        }
    }

    fn best(&self) -> Option<PeerId> {
        let t = Instant::now();
        self.known
            .iter()
            .filter(|(id, _)| self.banned.get(*id).is_none_or(|u| *u <= t))
            .max_by_key(|(id, p)| (p.rank, **id))
            .map(|(id, _)| *id)
    }

    fn rid(&mut self, peer: &PeerId) -> (u64, Option<u64>) {
        self.next_id += 1;
        let v = self.known.get(peer).map_or(100, |p| p.version);
        (self.next_id, (v == 101).then_some(self.next_id))
    }

    /// Starts a poll at the best peer when nothing is in flight.
    fn kick(&mut self) {
        if self.inflight.is_some() {
            return;
        }
        let Some(peer) = self.best() else { return };
        let (_, head) = self.core.head();
        self.ask(peer, Origin::Hash(head), Stage::Headers(head));
    }

    /// The lowest block a branch switch may start above: the committed block (V2), or `BATCH` below the head
    /// where nothing is committed (V1 has no finality; geth takes the newer branch there as well).
    fn fork_floor(&self, head: u64) -> u64 {
        self.core
            .chain
            .committed()
            .map_or(head.saturating_sub(BATCH), |c| c.number)
    }

    /// `BATCH + 1` headers from `origin`.
    fn ask(&mut self, peer: PeerId, origin: Origin, stage: Stage) {
        let (id, wire) = self.rid(&peer);
        let msg = Message::GetBlockHeaders {
            id: wire,
            query: GetHeaders {
                origin,
                amount: BATCH + 1,
                skip: 0,
                reverse: false,
            },
        };
        if self.peers.send(&peer, msg).is_ok() {
            self.inflight = Some(Inflight {
                peer,
                id,
                stage,
                deadline: Instant::now() + REQUEST_TIMEOUT,
            });
        }
    }

    /// The answer to `Stage::Fork`: the peer's chain from our committed block. A branch that leaves ours above
    /// it and ends above our head is fetched and adopted (`Core::adopt`).
    fn on_fork_headers(&mut self, peer: PeerId, headers: Vec<Header>) {
        self.inflight = None;
        let (number, _) = self.core.head();
        let at = {
            let h = self.core.hist.read();
            branch_start(&headers, |n| h.hash_at(n), number)
        };
        let at = match at {
            Ok(at) => at,
            // the peer has not imported our newest block yet (it just left us, or another peer gave it to
            // us first): propagation lag, not an offence; geth does not drop a peer for a head it lacks
            // a sibling of our head at its height is not a longer branch (geth keeps its head for it too)
            Err("no branch: every header is ours" | "the branch does not end above our head") => {
                return
            }
            Err(why) => return self.ban(peer, why),
        };
        let branch = headers[at..].to_vec();
        let (id, wire) = self.rid(&peer);
        let msg = Message::GetBlockBodies {
            id: wire,
            hashes: branch.iter().map(Header::hash).collect(),
        };
        if self.peers.send(&peer, msg).is_ok() {
            self.inflight = Some(Inflight {
                peer,
                id,
                stage: Stage::Branch(branch),
                deadline: Instant::now() + REQUEST_TIMEOUT,
            });
        }
    }

    /// The bodies of a branch: adopted in one step, or the peer is set aside.
    async fn on_branch_bodies(
        &mut self,
        peer: PeerId,
        mut headers: Vec<Header>,
        bodies: Vec<BlockBody>,
    ) {
        headers.truncate(bodies.len());
        let blocks: Vec<_> = headers
            .into_iter()
            .zip(bodies)
            .map(|(h, b)| (h, to_body(b).0))
            .collect();
        let core = self.core.clone();
        let r = tokio::task::spawn_blocking(move || core.adopt(blocks, now()))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.to_string()));
        match r {
            Ok((dropped, added)) => {
                let (n, h) = {
                    let g = self.core.hist.read();
                    (g.tip_number(), (g.tip().hash, g.td))
                };
                self.peers.set_head(h.0, h.1);
                self.progress.confirmed.store(false, Ordering::Relaxed);
                eprintln!(
                    "chain: adopted the branch of peer {}: {dropped} block(s) of ours dropped, {added} imported, head {n}",
                    short(&peer)
                );
                self.kick();
            }
            Err(e) => self.ban(peer, &format!("branch not adopted: {e}")),
        }
    }

    fn matches(&self, peer: &PeerId, id: Option<u64>) -> bool {
        self.inflight.as_ref().is_some_and(|i| {
            i.peer == *peer
                && id == (self.known.get(peer).is_some_and(|p| p.version == 101)).then_some(i.id)
        })
    }

    async fn import(&mut self, blocks: Vec<(Header, Body)>) -> Result<usize, String> {
        let core = self.core.clone();
        let txs: usize = blocks.iter().map(|(_, b)| b.transactions.len()).sum();
        let t0 = std::time::Instant::now();
        let r = tokio::task::spawn_blocking(move || {
            let mut n = 0;
            for (h, b) in blocks {
                // already canonical: a `NewBlock` delivered it while this answer travelled
                if core.hist.read().number_of(h.hash()) == Some(h.number) {
                    continue;
                }
                match core.import(h, b, now()) {
                    Ok(Outcome::Extended) => n += 1,
                    Ok(o) => return (n, Some(format!("not adopted: {o:?}"))),
                    Err(e) => return (n, Some(e.to_string())),
                }
            }
            (n, None)
        })
        .await
        .map_err(|e| e.to_string())?;
        let (head, _) = self.core.head();
        if r.0 > 0 {
            let (h, td) = {
                let g = self.core.hist.read();
                (g.tip().hash, g.td)
            };
            self.peers.set_head(h, td);
            self.progress.confirmed.store(false, Ordering::Relaxed);
            eprintln!(
                "chain: imported {} blocks ({txs} transactions) in {} ms, head {head}",
                r.0,
                t0.elapsed().as_millis()
            );
        }
        match r.1 {
            None => Ok(r.0),
            Some(e) => Err(e),
        }
    }

    async fn on_headers(&mut self, peer: PeerId, origin: B256, headers: Vec<Header>) {
        self.inflight = None;
        let (number, head) = self.core.head();
        match headers.first().map(Header::hash) {
            None => {
                // not yet there, or our head is a block of ours that the network left (sealed, then the
                // round timed out and the next leader built on the block below it): ask from the committed
                // block for its branch; an answer that is all ours is the "not yet" case and costs nothing
                let floor = self.fork_floor(number);
                if origin == head && floor < number {
                    return self.ask(peer, Origin::Number(floor), Stage::Fork);
                }
                return self.set_aside(peer, "does not know our head yet", NOT_YET);
            }
            Some(h) if h != origin => return self.ban(peer, "answered from another block"),
            _ => {}
        }
        let all = &headers[1..];
        // a `NewBlock` may have moved the head while the request travelled
        let rest = &all[all.iter().take_while(|h| h.number <= number).count()..];
        if rest.is_empty() {
            if !all.is_empty() || origin != head {
                return;
            }
            // an empty continuation proves "caught up" only from a peer that advertises our head or a block
            // we already hold (its last announcement is older than a block another peer gave us first: geth
            // announces each block once, `eth/handler_eth.go`)
            let holds = |h: B256| h == head || self.core.hist.read().number_of(h).is_some();
            if self.known.get(&peer).is_none_or(|p| !holds(p.head)) {
                // it announced a head we do not have and serves nothing above ours: not imported yet, or
                // our head is a side block of its chain (a V1 fork of two sealers at one height); its
                // chain from below tells which
                let floor = self.fork_floor(number);
                if floor < number {
                    self.ask(peer, Origin::Number(floor), Stage::Fork);
                } else {
                    self.set_aside(peer, "advertises a head it does not serve yet", NOT_YET);
                }
                return;
            }
            self.progress.confirmed.store(true, Ordering::Relaxed);
            self.progress.highest.fetch_max(number, Ordering::Relaxed);
            return;
        }
        let linked = headers
            .windows(2)
            .all(|w| w[1].parent_hash == w[0].hash() && w[1].number == w[0].number + 1);
        if !linked {
            self.ban(peer, "headers do not link");
            return;
        }
        self.progress
            .highest
            .fetch_max(rest[rest.len() - 1].number, Ordering::Relaxed);
        let (id, wire) = self.rid(&peer);
        let msg = Message::GetBlockBodies {
            id: wire,
            hashes: rest.iter().map(Header::hash).collect(),
        };
        if self.peers.send(&peer, msg).is_ok() {
            self.inflight = Some(Inflight {
                peer,
                id,
                stage: Stage::Bodies(rest.to_vec()),
                deadline: Instant::now() + REQUEST_TIMEOUT,
            });
        }
    }

    async fn on_bodies(&mut self, peer: PeerId, bodies: Vec<BlockBody>) {
        let headers = match self.inflight.take() {
            Some(Inflight {
                stage: Stage::Bodies(h),
                ..
            }) => h,
            Some(Inflight {
                stage: Stage::Branch(h),
                ..
            }) => return self.on_branch_bodies(peer, h, bodies).await,
            _ => return,
        };
        if bodies.is_empty() {
            self.ban(peer, "no bodies");
            return;
        }
        let mut headers = headers;
        let rest = headers.split_off(bodies.len().min(headers.len()));
        // A peer answers with what fits its response limit (2 MB in geth), often one dense block:
        // the bodies still missing are asked for before the ones at hand are imported, so the next
        // answer travels while this one executes.
        if !rest.is_empty() {
            let (id, wire) = self.rid(&peer);
            let msg = Message::GetBlockBodies {
                id: wire,
                hashes: rest.iter().map(Header::hash).collect(),
            };
            if self.peers.send(&peer, msg).is_ok() {
                self.inflight = Some(Inflight {
                    peer,
                    id,
                    stage: Stage::Bodies(rest),
                    deadline: Instant::now() + REQUEST_TIMEOUT,
                });
            }
        } else if let Some(last) = headers.last().map(Header::hash) {
            // the last bodies of the batch: the next headers travel while they import (geth
            // `eth/sync_xdc.go:2763-2829` fetches the next ranges while range 0 imports)
            self.ask(peer, Origin::Hash(last), Stage::Headers(last));
        }
        let blocks: Vec<_> = headers
            .into_iter()
            .zip(bodies)
            .map(|(h, b)| (h, to_body(b).0))
            .collect();
        if let Err(e) = self.import(blocks).await {
            self.ban(peer, &e);
            return;
        }
        // the deadline of a request sent before a long import counts from its end
        if let Some(i) = self.inflight.as_mut() {
            i.deadline = i.deadline.max(Instant::now() + REQUEST_TIMEOUT);
        }
        self.kick();
    }

    async fn on_new_block(&mut self, peer: PeerId, nb: Box<xone_net::eth::NewBlock>) {
        let rank = u64::try_from(nb.td).unwrap_or(u64::MAX);
        let h = nb.block.header;
        if let Some(p) = self.known.get_mut(&peer) {
            p.rank = p.rank.max(rank);
            p.head = h.hash();
        }
        self.progress.highest.fetch_max(h.number, Ordering::Relaxed);
        let (number, head) = self.core.head();
        if h.number <= number {
            return;
        }
        if h.parent_hash == head && h.number == number + 1 {
            let body = Body {
                transactions: nb.block.transactions.iter().map(from_wire).collect(),
                uncles: nb.block.uncles,
                withdrawals: nb
                    .block
                    .withdrawals
                    .map(|w| w.into_iter().map(|r| r.0).collect()),
            };
            if let Err(e) = self.import(vec![(h, body)]).await {
                self.ban(peer, &e);
            }
            return;
        }
        self.progress.confirmed.store(false, Ordering::Relaxed);
        self.kick();
    }

    async fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Connected {
                peer,
                version,
                rank,
                head,
                ..
            } => {
                eprintln!(
                    "chain: peer {} connected, eth/{version}, rank {rank}",
                    short(&peer)
                );
                self.known.insert(
                    peer,
                    Peer {
                        version,
                        rank,
                        head,
                    },
                );
                self.kick();
            }
            Event::Disconnected { peer } => {
                self.known.remove(&peer);
                // the proof was bound to a peer session
                self.progress.confirmed.store(false, Ordering::Relaxed);
                if self.inflight.as_ref().is_some_and(|i| i.peer == peer) {
                    self.inflight = None;
                }
                self.kick();
            }
            Event::Message { peer, msg } => match msg {
                Message::BlockHeaders { id, headers } if self.matches(&peer, id) => {
                    match self.inflight.as_ref().map(|i| &i.stage) {
                        Some(Stage::Headers(origin)) => {
                            let origin = *origin;
                            self.on_headers(peer, origin, headers).await;
                        }
                        Some(Stage::Fork) => self.on_fork_headers(peer, headers),
                        _ => {}
                    }
                }
                Message::BlockBodies { id, bodies } if self.matches(&peer, id) => {
                    self.on_bodies(peer, bodies).await;
                }
                Message::NewBlock(nb) => self.on_new_block(peer, nb).await,
                Message::NewBlockHashes(list) => {
                    if let Some(a) = list.iter().max_by_key(|a| a.number) {
                        let m = a.number;
                        if let Some(p) = self.known.get_mut(&peer) {
                            p.head = a.hash;
                        }
                        self.progress.highest.fetch_max(m, Ordering::Relaxed);
                        if m > self.core.head().0 {
                            self.progress.confirmed.store(false, Ordering::Relaxed);
                        }
                    }
                    self.kick();
                }
                Message::Transactions(raws) => self.pool.add_wire(&raws),
                // geth `Bfter.Vote`/`Timeout`/`SyncInfo` (`eth/bft/handler.go:187-229`): a validator verifies
                // and handles every BFT message of its peers. A node without `--mine` has no machine.
                Message::Bft(m) => {
                    if let Some(tx) = &self.bft {
                        let _ = tx.send((peer, m));
                    }
                }
                Message::Sub { sub, code, body } if Sub::is_request(code) => {
                    // a full queue drops the request; the peer times out and asks again
                    let _ = self.snap.try_send((peer, sub, code, body));
                }
                _ => {}
            },
        }
    }

    async fn run(mut self, mut events: Receiver<Event>, mut stop: watch::Receiver<bool>) {
        let mut tick = interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                ev = events.recv() => match ev {
                    Some(ev) => self.on_event(ev).await,
                    None => break,
                },
                _ = tick.tick() => {
                    if let Some(i) = &self.inflight {
                        if i.deadline <= Instant::now() {
                            let p = i.peer;
                            self.ban(p, "request timed out");
                        }
                    }
                    // a quiet follower polls once a second: that is how it learns of blocks that no
                    // peer announced, and the empty answer is the caught-up proof
                    self.kick();
                }
            }
        }
    }
}

fn short(p: &PeerId) -> String {
    alloy_primitives::hex::encode(&p[..4])
}

pub struct ChainService {
    core: Arc<Core>,
    rt: Handle,
    net: NetHandle,
    pool: Arc<PoolShared>,
    pub progress: Arc<Progress>,
    events: Option<Receiver<Event>>,
    bft: Option<Bft>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl ChainService {
    pub fn new(
        core: Arc<Core>,
        rt: Handle,
        net: NetHandle,
        pool: Arc<PoolShared>,
        events: Receiver<Event>,
        bft: Option<Bft>,
    ) -> Self {
        let progress = Arc::new(Progress::default());
        let (n, _) = core.head();
        progress.start.store(n, Ordering::Relaxed);
        progress.highest.store(n, Ordering::Relaxed);
        Self {
            core,
            rt,
            net,
            pool,
            progress,
            events: Some(events),
            bft,
            stop: watch::channel(false).0,
            task: None,
        }
    }
}

impl Service for ChainService {
    fn name(&self) -> &str {
        "chain"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let mut events = self
            .events
            .take()
            .ok_or_else(|| ServiceError("chain service restarted".into()))?;
        let (snap, rx) = mpsc::channel(64);
        self.rt
            .spawn(SnapServer::new(self.core.clone(), self.net.peers.clone()).run(rx));
        let f = Follower {
            snap,
            core: self.core.clone(),
            peers: self.net.peers.clone(),
            pool: self.pool.clone(),
            progress: self.progress.clone(),
            known: HashMap::new(),
            banned: HashMap::new(),
            next_id: 0,
            inflight: None,
            bft: self.bft.take(),
        };
        // Data requests are answered by a task of their own; the follower gets the rest.
        let server = Server {
            core: self.core.clone(),
            peers: self.net.peers.clone(),
            serving: Arc::new(Semaphore::new(SERVING)),
        };
        let (fwd, rest) = mpsc::channel(1024);
        let senders = self.core.senders.clone();
        self.rt.spawn(async move {
            while let Some(ev) = events.recv().await {
                // the senders of a block are recovered as it arrives, while the follower imports the
                // blocks before it (geth `SenderCacher`)
                if let Event::Message { msg, .. } = &ev {
                    let txs: Vec<Bytes> = match msg {
                        Message::BlockBodies { bodies, .. } => bodies
                            .iter()
                            .flat_map(|b| b.transactions.iter().map(from_wire))
                            .collect(),
                        Message::NewBlock(nb) => {
                            nb.block.transactions.iter().map(from_wire).collect()
                        }
                        _ => vec![],
                    };
                    senders.warm(txs);
                }
                match ev {
                    Event::Message {
                        peer,
                        msg:
                            m @ (Message::GetBlockHeaders { .. }
                            | Message::GetBlockBodies { .. }
                            | Message::GetReceipts { .. }),
                    } => server.serve(&peer, &m),
                    ev => {
                        if fwd.send(ev).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        self.task = Some(self.rt.spawn(f.run(rest, self.stop.subscribe())));
        let (n, h) = self.core.head();
        eprintln!("chain: following from block {n} {h}");
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.send_replace(true);
        if let Some(t) = self.task.take() {
            // a block being imported finishes first; the node's own deadline bounds the wait
            let _ = self
                .rt
                .block_on(async { timeout(Duration::from_millis(1500), t).await });
        }
        match self.core.checkpoint() {
            Ok(Some(n)) => eprintln!("chain: state checkpoint of block {n} written"),
            Ok(None) => {}
            Err(e) => eprintln!("chain: no checkpoint written: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` headers from block `from`, each the child of the one before; `salt` makes a different branch.
    fn chain(from: u64, n: u64, parent: B256, salt: u8) -> Vec<Header> {
        let mut out: Vec<Header> = Vec::new();
        let mut p = parent;
        for i in from..from + n {
            let h = Header {
                number: i,
                parent_hash: p,
                extra_data: vec![salt].into(),
                ..Header::default()
            };
            p = h.hash();
            out.push(h);
        }
        out
    }

    /// `n` hashes and a store that holds the first `held` of them, each with `txs` transactions of 1 KiB.
    fn range(n: u8, held: u8, txs: usize) -> (Vec<B256>, impl FnMut(B256) -> Option<Body>) {
        let hashes: Vec<_> = (0..n).map(B256::repeat_byte).collect();
        let have: Vec<_> = hashes[..usize::from(held)].to_vec();
        let get = move |h: B256| {
            have.contains(&h).then(|| Body {
                transactions: vec![vec![7u8; 1024].into(); txs],
                ..Body::default()
            })
        };
        (hashes, get)
    }

    /// geth `ServiceGetBlockBodiesQuery` (`eth/protocols/eth/handlers.go:260-277`): a peer that holds
    /// none of the range answers an empty list, one that holds the first blocks answers those and stops
    /// at the first gap, and the answer is cut at the soft limit and at 1024 blocks.
    #[test]
    fn a_peer_that_does_not_hold_the_range_answers_an_empty_list() {
        let (hashes, none) = range(128, 0, 1);
        assert!(bodies_answer(&hashes, none).is_empty());
        assert!(receipts_answer(&hashes, |_| None).is_empty());
        // about 1 MiB of receipts per block: the second crosses 2 MiB and is the last
        let big = |_| Some(vec![alloy_primitives::Bytes::from(vec![1u8; 1 << 20])]);
        assert_eq!(receipts_answer(&hashes, big).len(), 2);
        // holds 3 of 128, then a gap: the 3, never a later one against an earlier hash
        let (hashes, three) = range(128, 3, 1);
        assert_eq!(bodies_answer(&hashes, three).len(), 3);
        // 100 blocks of just over 1 MiB: the second crosses 2 MiB, it is the last (geth: `bytes >= softResponseLimit` is
        // checked before the next lookup)
        let (hashes, big) = range(100, 100, 1024);
        let out = bodies_answer(&hashes, big);
        assert_eq!(out.len(), 2);
        // 2000 hashes: at most 1024 are looked up
        let hashes: Vec<_> = (0..2000u32)
            .map(|i| B256::left_padding_from(&i.to_be_bytes()))
            .collect();
        assert_eq!(
            bodies_answer(&hashes, |_| Some(Body::default())).len(),
            MAX_SERVE
        );
        assert_eq!(receipts_answer(&hashes, |_| Some(vec![])).len(), MAX_SERVE);
    }

    /// The restart case of the gate D rehearsal: we hold 100..=103, the last of them our own block that the
    /// network left; the peer serves 100..=102 then its own 103', 104', 105'. The branch starts at 103' and
    /// is adopted; a branch that does not end above our head, a gap, or a foreign start are refused.
    #[test]
    fn a_longer_branch_is_found_above_our_committed_block() {
        let base = chain(100, 3, B256::repeat_byte(7), 0);
        let mut ours = base.clone();
        ours.extend(chain(103, 1, base[2].hash(), 1));
        let lookup = |n: u64| ours.iter().find(|h| h.number == n).map(Header::hash);
        let mut theirs = base.clone();
        theirs.extend(chain(103, 3, base[2].hash(), 2));
        assert_eq!(branch_start(&theirs, lookup, 103), Ok(3));
        // as long as ours: no switch
        assert!(branch_start(&theirs[..4], lookup, 103).is_err());
        // a header missing in the middle
        let mut gap = theirs.clone();
        gap.remove(4);
        assert_eq!(branch_start(&gap, lookup, 103), Err("headers do not link"));
        // not from our chain
        let foreign = chain(100, 6, B256::repeat_byte(9), 3);
        assert!(branch_start(&foreign, lookup, 103).is_err());
        // our own chain extended: the "branch" starts above our head and drops nothing
        let mut ext = ours.clone();
        ext.extend(chain(104, 1, ours[3].hash(), 1));
        assert_eq!(branch_start(&ext, lookup, 103), Ok(4));
    }
}
