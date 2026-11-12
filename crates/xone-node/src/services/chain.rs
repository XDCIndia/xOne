//! The `chain` service: a minimal live-follow driver over `xone-net` that the XB-14 pipeline replaces.
//!
//! It keeps no schedule of its own. The best peer by TD rank is asked for the headers that follow our
//! head (`GetBlockHeaders` from our head hash, so that the first answer must be our head: a peer that
//! does not know it is on another chain and is put aside), then for the bodies of those headers; the
//! blocks go through `Core::import` in order. An empty continuation from the best peer is the
//! caught-up proof (hash equality: the peer's chain holds our head and nothing above it). `NewBlock`
//! that extends the head is imported at once. Receipts are not requested: the node executes every
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

use alloy_primitives::B256;
use tokio::{
    runtime::Handle,
    sync::{
        mpsc::{self, Receiver},
        watch,
    },
    task::JoinHandle,
    time::{interval, timeout, Instant},
};
use xone_consensus::verify::Body;
use xone_net::{
    eth::{BlockBody, GetHeaders, Message, Origin, Raw},
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
    Headers,
    Bodies(Vec<Header>),
}

struct Inflight {
    peer: PeerId,
    id: u64,
    stage: Stage,
    deadline: Instant,
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
}

/// One block's receipts as eth carries them: a list whose items are the consensus encodings, a
/// typed receipt wrapped in an RLP string (the same rule as a transaction, [`to_wire`]).
fn receipts_wire(receipts: &[alloy_primitives::Bytes]) -> Raw {
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
        eprintln!("chain: setting peer {} aside for 60 s: {why}", short(&p));
        self.banned.insert(p, Instant::now() + BAN);
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
        let (id, wire) = self.rid(&peer);
        let (_, head) = self.core.head();
        let msg = Message::GetBlockHeaders {
            id: wire,
            query: GetHeaders {
                origin: Origin::Hash(head),
                amount: BATCH + 1,
                skip: 0,
                reverse: false,
            },
        };
        if self.peers.send(&peer, msg).is_ok() {
            self.inflight = Some(Inflight {
                peer,
                id,
                stage: Stage::Headers,
                deadline: Instant::now() + REQUEST_TIMEOUT,
            });
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
        let r = tokio::task::spawn_blocking(move || {
            let mut n = 0;
            for (h, b) in blocks {
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
            eprintln!("chain: imported {} blocks, head {head}", r.0);
        }
        match r.1 {
            None => Ok(r.0),
            Some(e) => Err(e),
        }
    }

    async fn on_headers(&mut self, peer: PeerId, headers: Vec<Header>) {
        self.inflight = None;
        let (number, head) = self.core.head();
        if headers.first().map(Header::hash) != Some(head) {
            self.ban(peer, "does not know our head");
            return;
        }
        let rest = &headers[1..];
        if rest.is_empty() {
            // an empty continuation proves "caught up" only from a peer that advertises our head;
            // a peer that claims more and serves nothing is set aside
            if self.known.get(&peer).is_none_or(|p| p.head != head) {
                self.ban(peer, "advertises a head it does not serve");
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
        let Some(Inflight {
            stage: Stage::Bodies(headers),
            ..
        }) = self.inflight.take()
        else {
            return;
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
        if h.parent_hash == head && h.number == number + 1 && self.inflight.is_none() {
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

    /// Answers a header or body request of a peer from the history.
    fn serve(&self, peer: &PeerId, msg: &Message) {
        let h = self.core.hist.read();
        let reply = match msg {
            Message::GetBlockHeaders { id, query } => {
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
                let found: Vec<_> = hashes
                    .iter()
                    .take(128)
                    .filter_map(|x| Some((h.number_of(*x)?, *x)))
                    .collect();
                drop(h);
                Message::BlockBodies {
                    id: *id,
                    bodies: found
                        .into_iter()
                        .filter_map(|(n, x)| self.core.records(n, x))
                        .map(|(b, _)| BlockBody {
                            transactions: b.transactions.iter().map(to_wire).collect(),
                            uncles: b.uncles,
                            withdrawals: b
                                .withdrawals
                                .map(|w| w.into_iter().map(xone_net::eth::Raw).collect()),
                        })
                        .collect(),
                }
            }
            Message::GetReceipts { id, hashes, .. } => {
                let found: Vec<_> = hashes
                    .iter()
                    .take(128)
                    .filter_map(|x| Some((h.number_of(*x)?, *x)))
                    .collect();
                drop(h);
                Message::Receipts {
                    id: *id,
                    last_incomplete: false,
                    per_block: found
                        .into_iter()
                        .filter_map(|(n, x)| self.core.records(n, x))
                        .map(|(_, r)| receipts_wire(&r))
                        .collect(),
                }
            }
            _ => return,
        };
        // a full queue of the peer drops the answer; it asks again
        let _ = self.peers.send(peer, reply);
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
                    if matches!(
                        self.inflight.as_ref().map(|i| &i.stage),
                        Some(Stage::Headers)
                    ) {
                        self.on_headers(peer, headers).await;
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
                m @ (Message::GetBlockHeaders { .. }
                | Message::GetBlockBodies { .. }
                | Message::GetReceipts { .. }) => {
                    self.serve(&peer, &m);
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
        let events = self
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
        };
        self.task = Some(self.rt.spawn(f.run(events, self.stop.subscribe())));
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
