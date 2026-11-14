//! `--syncmode snap` (XB-17): an empty datadir is filled from peers at a pivot block and installed
//! through the snapshot install path, then the node starts as an anchored node and executes from
//! pivot + 1 (`spec/snap/pivot.md`, `spec/bootstrap/snap-install.md`).
//!
//! In order, each step verified before the next:
//!
//! 1. Peers: the node's own net service on its own runtime. State comes from a peer that shares
//!    snap/1 (geth-XDC included: the store is keyed by hash like snap/1, no preimages are needed);
//!    none after the wait is [`SnapSyncError::NoSnap`], never a fall back to full sync.
//! 2. Pivot: the head of the best snap/1 peer minus 64 (geth `fsMinFullBlocks`,
//!    `eth/downloader/downloader.go`), lowered to the first block whose anchor proof verifies
//!    (`verify_anchor`: the epoch-switch header's masternodes certify the pivot by the 3-chain).
//!    The switch header is pinned by the operator's pins file, or by the majority of at least two peers
//!    that serve the same header for its number; a majority against it is [`SnapSyncError::Disputed`].
//! 3. The header window and the records (bodies and receipts) of `plan_at(pivot)`, linked down from
//!    the anchor and checked against their headers (`BodyVerifier`).
//! 4. The candidate pool of every gap block of the plan, from the 0x88 account and storage at that
//!    block's state root, each range proven (`xone_snap::client::account_at`).
//! 5. The pivot state (`xone_snap::client::sync`: ranges with proofs, codes by hash, heal, root),
//!    then the state root rebuilt from the hashed rows. When the peers' state window leaves the
//!    pivot ("no data"), steps 2 to 5 run again for a fresh pivot over the downloaded state
//!    ([`repivot`]).
//! 6. `snapshot::install`: the state checkpoint, then `Store::install_snapshot` in one commit.
//!
//! ponytail: one source peer at a time, the downloaded state in memory, a stop or a crash before the
//! install starts again from nothing (the datadir is still empty). Parallel peers and a persisted
//! `StateStore` when a network's state outgrows memory. A move fetches the header window and the gap
//! pools again; cache them when a network needs many moves.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    time::{Duration, Instant},
};

use super::gaprpc;
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::Decodable;
use tokio::{runtime::Handle, sync::mpsc::Receiver, time::timeout};
use xone_chainspec::{ChainSpec, Genesis};
use xone_consensus::{
    chain::signed_hash,
    v2::gap_snapshot,
    verify::{Body, BodyVerifier, Receipt},
};
use xone_net::{
    eth::{GetHeaders, Message as Eth, Origin, Raw},
    peers::{Event, Peers},
    rlpx::{PeerId, Sub},
};
use xone_primitives::Header;
use xone_snap::{
    client::{self, Backend, Fault, PeerError, PeerSource, StateStore, SyncError},
    codec::Message as Snap,
    preimage::{self, GetPreimages},
};
use xone_state::{system_contracts::validator_0x88, Account, StateRead};
use xone_sync::{
    bootstrap::{
        manifest::{build_anchor, parse_pins, Side, SignRow},
        plan_at, verify_anchor, Anchor, AnchorError, Class, Context,
    },
    store::{Block, Install, Record, StateRows},
};

use super::{
    chain::to_body,
    core::{now, RETENTION},
    from_wire,
    net::{load_or_create_key, NetService},
    short, store,
};
use crate::{
    config::Config,
    lifecycle::{Service, Shutdown},
};

/// geth `fsMinFullBlocks` (`eth/downloader/downloader.go`): the pivot stands this far below the head.
pub const PIVOT_DEPTH: u64 = 64;
/// Blocks below the first candidate searched for a pivot whose anchor verifies.
const PIVOT_SEARCH: u64 = 64;
/// How long to wait for peers before giving up.
const PEER_WAIT: Duration = Duration::from_secs(120);
const ETH_TIMEOUT: Duration = Duration::from_secs(15);
/// A server builds its snapshot of a root on the first request for it.
const SNAP_TIMEOUT: Duration = Duration::from_secs(120);
/// Headers and bodies per request (the xOne server's caps, `chain.rs`).
const HEADERS: u64 = 192;
const BODIES: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum SnapSyncError {
    #[error("stopped before the install; the datadir is still empty")]
    Stopped,
    #[error("no peer after {0:?}")]
    NoPeers(Duration),
    #[error("no peer serves snap/1 after {0:?}: snap sync needs one, refusing (not falling back to full sync)")]
    NoSnap(Duration),
    #[error("peer {peer}: {why}")]
    Peer { peer: String, why: String },
    #[error("no pivot within {PIVOT_SEARCH} blocks below {from} has a verified anchor: {last}")]
    NoPivot { from: u64, last: String },
    #[error("anchor of block {0}: {1}")]
    Anchor(u64, AnchorError),
    #[error(
        "switch header {number}: {have} peer(s) serve {hash}, {need} needed (or pass --snap.pins)"
    )]
    Witnesses {
        number: u64,
        hash: B256,
        have: usize,
        need: usize,
    },
    #[error("switch header {number}: peer {peer} serves {theirs}, others {ours}; refusing")]
    Disputed {
        number: u64,
        peer: String,
        ours: B256,
        theirs: B256,
    },
    #[error("--snap.pins {0}")]
    Pins(String),
    /// The peer's chain below the pivot is not the required one (`--eth.requiredblocks`).
    #[error("{0}: the chain of the pivot is on another branch")]
    RequiredBlock(#[from] xone_chainspec::RequiredBlockMismatch),
    #[error("block {number}: {why}")]
    Block { number: u64, why: String },
    #[error(
        "the pivot {0} is a V1 block or its gap pools are V1 lists: snap sync starts on V2 only"
    )]
    V1(u64),
    #[error("gap block {number}: {why}")]
    Gap { number: u64, why: String },
    #[error("gap block {number}: no source for its 0x88 state. Geth serves snap/1 only for recent state, so a geth-only peer set cannot supply it. Either connect an xOne peer (snap/1 serves it from its 4,096-block window) or pass --snap.gap-rpc <url> of any archive RPC that serves eth_getProof (every proof is verified against the header; the RPC is not trusted)")]
    GapSource { number: u64 },
    #[error("state: {0}")]
    State(#[from] SyncError),
    #[error("the account with key {0} holds more than 2^256 and no xOne peer (xpre/1) told its address; the executor needs the address of such an account, so this sync cannot continue")]
    WideAddress(B256),
    #[error("state root {got} rebuilt from the hashed rows, the pivot header says {want}")]
    Root { got: B256, want: B256 },
    #[error("{0}")]
    Io(String),
}

type R<T> = Result<T, SnapSyncError>;

struct Peer {
    version: u64,
    head: B256,
    /// The total difficulty of the Status, at `head` (saturated at `u64::MAX`).
    rank: u64,
    subs: Vec<Sub>,
}

#[derive(Default)]
struct Counts {
    account_ranges: u64,
    storage_ranges: u64,
    byte_codes: u64,
    trie_nodes: u64,
    snap_bytes: u64,
}

struct Drv<'a> {
    h: Handle,
    peers: Peers,
    events: Receiver<Event>,
    known: BTreeMap<PeerId, Peer>,
    next_id: u64,
    stop: &'a Shutdown,
    /// Faults per peer: the source is the snap/1 peer with the fewest.
    faults: HashMap<PeerId, u32>,
    n: Counts,
    /// The last snap/1 request and its answer, shown with a proof fault.
    last: Option<(Snap, Snap)>,
    /// Where a proof fault writes that request and answer in full.
    dump: std::path::PathBuf,
}

fn bad(peer: &PeerId, why: impl std::fmt::Display) -> SnapSyncError {
    SnapSyncError::Peer {
        peer: short(peer),
        why: why.to_string(),
    }
}

impl Drv<'_> {
    fn on_event(&mut self, ev: Event) -> Option<(PeerId, Eth)> {
        match ev {
            Event::Connected {
                peer,
                version,
                rank,
                head,
                subs,
                ..
            } => {
                let caps: Vec<String> = subs
                    .iter()
                    .map(|s| format!("{}/{}", s.cap().name, s.cap().version))
                    .collect();
                eprintln!(
                    "snap: peer {} connected, eth/{version}, shared {}",
                    short(&peer),
                    if caps.is_empty() {
                        "eth only".to_owned()
                    } else {
                        caps.join(" ")
                    }
                );
                self.known.insert(
                    peer,
                    Peer {
                        version,
                        head,
                        rank,
                        subs,
                    },
                );
                None
            }
            Event::Disconnected { peer } => {
                self.known.remove(&peer);
                None
            }
            Event::Message { peer, msg } => Some((peer, msg)),
        }
    }

    /// The next message of a peer before `until`; `None` at the deadline.
    fn next(&mut self, until: Instant) -> R<Option<(PeerId, Eth)>> {
        loop {
            if self.stop.is_requested() {
                return Err(SnapSyncError::Stopped);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let step = left.min(Duration::from_millis(500));
            let ev = self
                .h
                .block_on(async { timeout(step, self.events.recv()).await });
            match ev {
                Err(_) => {}
                Ok(None) => return Err(SnapSyncError::Io("the net service stopped".into())),
                Ok(Some(ev)) => {
                    if let Some(m) = self.on_event(ev) {
                        return Ok(Some(m));
                    }
                }
            }
        }
    }

    fn has(&self, p: &PeerId, s: Sub) -> bool {
        self.known.get(p).is_some_and(|x| x.subs.contains(&s))
    }

    /// Waits until a peer shares snap/1 (and, for the pin, until `others` more peers are connected
    /// or the wait is over).
    fn wait_peers(&mut self, others: usize) -> R<()> {
        let until = Instant::now() + PEER_WAIT;
        let ready =
            |d: &Self| d.known.keys().any(|p| d.has(p, Sub::Snap)) && d.known.len() > others;
        while !ready(self) {
            if self.next(until)?.is_none() {
                break;
            }
        }
        if self.known.is_empty() {
            return Err(SnapSyncError::NoPeers(PEER_WAIT));
        }
        if self.source().is_none() {
            return Err(SnapSyncError::NoSnap(PEER_WAIT));
        }
        Ok(())
    }

    /// The peer that serves state: snap/1, fewest faults.
    fn source(&self) -> Option<PeerId> {
        self.pick(false)
    }

    /// `xone`: only peers that also offer xpre/1 (xOne nodes, which keep older state in their window).
    fn pick(&self, xone: bool) -> Option<PeerId> {
        self.known
            .keys()
            .filter(|p| self.has(p, Sub::Snap) && (!xone || self.has(p, Sub::Xpre)))
            .min_by_key(|p| (self.faults.get(*p).copied().unwrap_or(0), **p))
            .copied()
    }

    fn src(&self) -> R<PeerId> {
        self.source().ok_or(SnapSyncError::NoSnap(Duration::ZERO))
    }

    fn fault(&mut self, p: &PeerId) {
        *self.faults.entry(*p).or_default() += 1;
    }

    /// One eth request and the answer of the same kind and id (eth/101) from that peer.
    fn eth(&mut self, peer: PeerId, mk: impl FnOnce(Option<u64>) -> Eth) -> R<Eth> {
        self.next_id += 1;
        let v101 = self.known.get(&peer).is_some_and(|p| p.version == 101);
        let id = v101.then_some(self.next_id);
        let req = mk(id);
        let want = match &req {
            Eth::GetBlockHeaders { .. } => 0,
            Eth::GetBlockBodies { .. } => 1,
            _ => 2,
        };
        self.peers
            .send(&peer, req)
            .map_err(|e| bad(&peer, format!("send: {e}")))?;
        let until = Instant::now() + ETH_TIMEOUT;
        while let Some((from, m)) = self.next(until)? {
            if from != peer {
                continue;
            }
            let hit = match &m {
                Eth::BlockHeaders { id: i, .. } => want == 0 && *i == id,
                Eth::BlockBodies { id: i, .. } => want == 1 && *i == id,
                Eth::Receipts { id: i, .. } => want == 2 && *i == id,
                _ => false,
            };
            if hit {
                return Ok(m);
            }
        }
        self.fault(&peer);
        Err(bad(&peer, "eth request timed out"))
    }

    fn headers_from(&mut self, peer: PeerId, origin: Origin, amount: u64) -> R<Vec<Header>> {
        match self.eth(peer, |id| Eth::GetBlockHeaders {
            id,
            query: GetHeaders {
                origin,
                amount,
                skip: 0,
                reverse: false,
            },
        })? {
            Eth::BlockHeaders { headers, .. } => Ok(headers),
            _ => Err(bad(&peer, "not headers")),
        }
    }

    /// One snap/1 or xpre/1 exchange with `peer`: the answer is the next message of that capability
    /// whose request id is `id`.
    fn sub(
        &mut self,
        peer: PeerId,
        sub: Sub,
        code: u64,
        body: Vec<u8>,
        id: u64,
    ) -> Result<(u64, Bytes), PeerError> {
        self.n.snap_bytes += u64::try_from(body.len()).unwrap_or(0);
        self.peers
            .send(
                &peer,
                Eth::Sub {
                    sub,
                    code,
                    body: body.into(),
                },
            )
            .map_err(|_| PeerError::Disconnected)?;
        let until = Instant::now() + SNAP_TIMEOUT;
        loop {
            let next = self.next(until).map_err(|_| PeerError::Disconnected)?;
            let Some((from, m)) = next else {
                return Err(PeerError::Timeout);
            };
            if !self.known.contains_key(&peer) {
                return Err(PeerError::Disconnected);
            }
            let Eth::Sub {
                sub: s,
                code: c,
                body,
            } = m
            else {
                continue;
            };
            if from != peer || s != sub || Sub::is_request(c) {
                continue;
            }
            let got = match sub {
                Sub::Snap => Snap::decode(c, &body).map(|m| m.id()).ok(),
                Sub::Xpre => preimage::Message::decode(c, &body).map(|m| m.id()).ok(),
            };
            if got == Some(id) {
                self.n.snap_bytes += u64::try_from(body.len()).unwrap_or(0);
                return Ok((c, body));
            }
        }
    }
}

/// The snap/1 source of `xone_snap::client`: the current snap/1 peer; a fault moves to the next.
struct Src<'a, 'b>(&'a mut Drv<'b>, bool);

impl PeerSource for Src<'_, '_> {
    fn exchange(&mut self, req: Snap) -> Result<Snap, PeerError> {
        let d = &mut *self.0;
        let peer = d.pick(self.1).ok_or(PeerError::Disconnected)?;
        let c = &mut d.n;
        match req {
            Snap::GetAccountRange(_) => c.account_ranges += 1,
            Snap::GetStorageRanges(_) => c.storage_ranges += 1,
            Snap::GetByteCodes(_) => c.byte_codes += 1,
            Snap::GetTrieNodes(_) => c.trie_nodes += 1,
            _ => {}
        }
        let (code, body) = d.sub(peer, Sub::Snap, req.code(), req.encode(), req.id())?;
        let reply = Snap::decode(code, &body).map_err(|_| PeerError::Disconnected)?;
        d.last = Some((req, reply.clone()));
        Ok(reply)
    }

    fn report(&mut self, fault: &Fault) {
        if let Some(p) = self.0.pick(self.1) {
            eprintln!("snap: peer {}: {fault}", short(&p));
            if let (Fault::Proof(_), Some((q, a))) = (fault, &self.0.last) {
                let clip = |x: String| x.chars().take(1500).collect::<String>();
                let _ = std::fs::write(&self.0.dump, format!("{fault}\n{q:?}\n{a:?}\n"));
                eprintln!(
                    "snap:   request {}\nsnap:   answer {}",
                    clip(format!("{q:?}")),
                    clip(format!("{a:?}"))
                );
            }
            self.0.fault(&p);
        }
    }
}

/// The 0x88 state of one gap block, for `gap_snapshot::build`. Slots by hashed key.
struct One {
    account: Account,
    slots: BTreeMap<B256, U256>,
}

impl StateRead for One {
    fn account(&self, a: Address) -> Option<Account> {
        (a == validator_0x88::ADDRESS).then(|| self.account.clone())
    }
    fn storage(&self, a: Address, slot: B256) -> U256 {
        if a != validator_0x88::ADDRESS {
            return U256::ZERO;
        }
        self.slots
            .get(&keccak256(slot))
            .copied()
            .unwrap_or(U256::ZERO)
    }
}

/// One block's receipts from the eth list: each item is a consensus encoding (a typed receipt
/// inside an RLP string, as a transaction is).
fn receipts_of(raw: &Raw) -> Result<Vec<Bytes>, alloy_rlp::Error> {
    let mut b = &raw.0[..];
    let h = alloy_rlp::Header::decode(&mut b)?;
    if !h.list || b.len() != h.payload_length {
        return Err(alloy_rlp::Error::UnexpectedString);
    }
    let mut out = Vec::new();
    while !b.is_empty() {
        out.push(receipt_of(&Raw::decode(&mut b)?)?);
    }
    Ok(out)
}

/// One receipt of the wire list as its consensus encoding. Two shapes arrive: xOne serves the
/// consensus form (a typed receipt inside an RLP string, a legacy one as the list `[status, cumulative
/// gas, bloom, logs]`), and geth-XDC serves eth/69's, with no bloom: the list `[type, status or
/// post-state, cumulative gas, logs]` (`eth/protocols/eth/receipt.go:31-36`, answered even on the
/// XDPOS2 wire, `handlers.go:337-345`). The bloom is derived from the logs, as the body check does.
fn receipt_of(r: &Raw) -> Result<Bytes, alloy_rlp::Error> {
    let mut b = &r.0[..];
    let h = alloy_rlp::Header::decode(&mut b)?;
    if !h.list {
        return Ok(from_wire(r));
    }
    // the third item is a 256-byte bloom in the consensus form and the gas used in eth/69's
    let mut probe = b;
    let (_, _, third) = (
        alloy_rlp::Header::decode_bytes(&mut probe, false)?,
        alloy_rlp::Header::decode_bytes(&mut probe, false)?,
        alloy_rlp::Header::decode_bytes(&mut probe, false)?,
    );
    if third.len() == 256 {
        return Ok(from_wire(r));
    }
    let tx_type = u8::decode(&mut b)?;
    let post_state_or_status = Bytes::decode(&mut b)?;
    let cumulative_gas_used = u64::decode(&mut b)?;
    let logs = Vec::<alloy_primitives::Log>::decode(&mut b)?;
    if !b.is_empty() {
        return Err(alloy_rlp::Error::UnexpectedLength);
    }
    Ok(Bytes::from(
        Receipt {
            tx_type,
            post_state_or_status,
            cumulative_gas_used,
            logs,
        }
        .encode_consensus(),
    ))
}

/// The sign transactions of a block, as the executor records them (`xone_executor`:
/// `signed_hash` of each transaction, the sender, the receipt status).
fn signs_of(chain_id: u64, number: u64, body: &Body, receipts: &[Bytes]) -> R<Vec<SignRow>> {
    let err = |why: String| SnapSyncError::Block { number, why };
    let mut out = Vec::new();
    for (raw, r) in body.transactions.iter().zip(receipts) {
        let t = xone_executor::tx::decode(raw, chain_id).map_err(|e| err(e.to_string()))?;
        if let Some(signed) = signed_hash(t.to, &t.data) {
            let r = Receipt::decode(r).map_err(|e| err(e.to_string()))?;
            out.push(SignRow {
                sender: t.from,
                signed,
                ok: r.post_state_or_status[..] == [1u8],
            });
        }
    }
    Ok(out)
}

/// The datadir holds nothing yet: snap sync applies. A datadir that holds a chain continues it.
pub fn is_fresh(datadir: &Path) -> Result<bool, String> {
    let dir = datadir.join("xone");
    if !dir.join("head.json").exists() && !dir.exists() {
        return Ok(true);
    }
    let s = store::open(&dir, RETENTION, None, false).map_err(|e| e.to_string())?;
    Ok(store::load(&s).map_err(|e| e.to_string())?.is_none())
}

/// Runs snap sync into the empty datadir of `cfg`, then returns; the caller builds the node, which
/// opens the anchored datadir. `stamp` is the p2p client name.
pub fn run(
    cfg: &Config,
    spec: &ChainSpec,
    genesis: &Genesis,
    stamp: &str,
    stop: &Shutdown,
) -> R<()> {
    let io = |e: String| SnapSyncError::Io(e);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("xone-snap")
        .enable_all()
        .build()
        .map_err(|e| io(e.to_string()))?;
    let key = load_or_create_key(&cfg.datadir).map_err(io)?;
    let mut pc = xone_net::peers::Config::new(
        stamp,
        cfg.network_id.unwrap_or(spec.chain_id),
        spec.genesis_hash,
    );
    pc.max_peers = cfg.p2p.max_peers as usize;
    pc.restrict = cfg.netrestrict().map_err(io)?;
    pc.required_blocks = cfg
        .required_blocks_of_chain(spec.chain_id)
        .map_err(|e| io(e.to_string()))?;
    pc.shutdown_grace = Duration::from_secs(1);
    let g = genesis
        .to_header(spec)
        .map_err(|e| io(format!("genesis: {e}")))?;
    pc.head = g.hash();
    pc.td = g.difficulty;
    let (peers, events) = Peers::new(pc.clone(), key);
    super::snapview::set_peers(&peers);
    let mut net = NetService::new(cfg, rt, peers.clone(), key, &pc).map_err(io)?;
    net.start().map_err(|e| io(e.to_string()))?;
    let h = net.handle().runtime();
    let mut d = Drv {
        h,
        peers,
        events,
        known: BTreeMap::new(),
        next_id: 0,
        stop,
        faults: HashMap::new(),
        n: Counts::default(),
        last: None,
        dump: cfg.datadir.join("snap-fault.txt"),
    };
    let r = sync(&mut d, cfg, spec, genesis);
    net.stop();
    r
}

fn sync(d: &mut Drv<'_>, cfg: &Config, spec: &ChainSpec, genesis: &Genesis) -> R<()> {
    let t0 = Instant::now();
    let pins = match &cfg.sync.snap_pins {
        Some(p) => parse_pins(
            &std::fs::read_to_string(p)
                .map_err(|e| SnapSyncError::Pins(format!("{}: {e}", p.display())))?,
        )
        .map_err(|e| SnapSyncError::Pins(format!("{}: {e}", p.display())))?,
        None => vec![],
    };
    eprintln!("snap: --syncmode snap on an empty datadir: state download at a pivot (snap/1)");
    d.wait_peers(if pins.is_empty() { 1 } else { 0 })?;

    // ---- the head, the pivot, the window, the gap pools and the pivot state. A pivot that the peers'
    // state window has left (every peer answers "no data": `SyncError::Unavailable`) is replaced by a
    // fresh one and the download goes on over what it holds, as geth's syncer does when its pivot
    // moves (`set_root`; the heal pass of `client::sync` repairs what the new root changed).
    let scfg = client::Config::default();
    let t1 = Instant::now();
    let disk = cfg.sync.state_disk;
    let dir = cfg.datadir.join("xone");
    std::fs::create_dir_all(&dir).map_err(|e| SnapSyncError::Io(e.to_string()))?;
    let mut pivot_fn = |d: &mut Drv<'_>| -> R<(Pivoted, B256)> {
        let p = pivot_window(d, cfg, spec, &pins)?;
        eprintln!(
            "snap: state download at root {} from peer {}",
            p.anchor.state_root,
            short(&d.src()?)
        );
        let root = p.anchor.state_root;
        Ok((p, root))
    };
    // With `--state.disk` the rows are proven straight into the state tables of the store
    // (`SnapState`), with their cursors, so that the download holds no state in memory and a crash
    // resumes it; the install then writes the rest. Otherwise the state is held in memory.
    let (pivoted, rows, rows_in_tables, wide, codes, mut store, got) = if disk {
        let store = store::open(&dir, RETENTION, None, true)
            .map_err(|e| SnapSyncError::Io(e.to_string()))?;
        let (pivoted, st) = repivot_with(
            d,
            &mut pivot_fn,
            |root| {
                store
                    .snap_state(root)
                    .map_err(|e| SnapSyncError::Io(e.to_string()))
            },
            |st, d| client::sync(st, &mut Src(d, false), &scfg),
        )?;
        let want = pivoted.anchor.state_root;
        let io = |e: xone_snap::client::StoreError| SnapSyncError::Io(e.to_string());
        let got = st.root().map_err(io)?;
        if got != want || st.progress().verified != Some(want) {
            return Err(SnapSyncError::Root { got, want });
        }
        eprintln!(
            "snap: state downloaded in {} s: {}; requests: {} account ranges, {} storage ranges, {} bytecodes, {} trie nodes (heal); every range proven; state root {got} streamed from the tables",
            t1.elapsed().as_secs(),
            st.summary(),
            d.n.account_ranges,
            d.n.storage_ranges,
            d.n.byte_codes,
            d.n.trie_nodes,
        );
        let wide = st.wide_accounts().map_err(io)?;
        let codes = st.codes().map_err(io)?;
        (pivoted, StateRows::default(), true, wide, codes, store, got)
    } else {
        let (pivoted, mut st) = repivot(d, &mut pivot_fn, |st, d| {
            client::sync(st, &mut Src(d, false), &scfg)
        })?;
        let slots: usize = st.storage.values().map(BTreeMap::len).sum();
        let c = &d.n;
        eprintln!(
            "snap: state downloaded in {} s: {} accounts, {slots} slots, {} codes; requests: {} account ranges, {} storage ranges, {} bytecodes, {} trie nodes (heal); every range proven",
            t1.elapsed().as_secs(),
            st.accounts.len(),
            st.codes.len(),
            c.account_ranges,
            c.storage_ranges,
            c.byte_codes,
            c.trie_nodes,
        );
        // ---- the rows, by hashed key as snap/1 gives them. The state root is checked from the hashed
        // rows by the streaming builder (`hashed_root`, no copy of the state), then the rows are moved
        // out of the store, not copied: a network's state is held once.
        let got = xone_snap::server::hashed_root(&st.accounts, &st.storage);
        if got != pivoted.anchor.state_root
            || st.progress.verified != Some(pivoted.anchor.state_root)
        {
            return Err(SnapSyncError::Root {
                got,
                want: pivoted.anchor.state_root,
            });
        }
        let mut rows = StateRows {
            accounts: std::mem::take(&mut st.accounts)
                .into_iter()
                .map(|(h, a)| {
                    (
                        h,
                        Account {
                            nonce: a.nonce,
                            balance: a.balance,
                            code_hash: a.code_hash,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Default::default()
        };
        let storage = std::mem::take(&mut st.storage);
        rows.storage
            .reserve_exact(storage.values().map(BTreeMap::len).sum());
        for (h, slots) in storage {
            rows.storage
                .extend(slots.into_iter().map(|(k, v)| (h, k, v)));
        }
        let wide: Vec<B256> = rows
            .accounts
            .iter()
            .filter(|(_, x)| x.balance.is_wide())
            .map(|(h, _)| *h)
            .collect();
        let store = store::open(&dir, RETENTION, None, false)
            .map_err(|e| SnapSyncError::Io(e.to_string()))?;
        let codes = st.codes.clone();
        (pivoted, rows, false, wide, codes, store, got)
    };
    let Pivoted {
        anchor,
        td,
        pin_source,
        win,
        records,
        mut side,
        now,
    } = pivoted;
    let pivot = anchor.number;
    let pv = &anchor.provenance;
    let c = &d.n;
    eprintln!(
        "snap: heal: {} trie-node requests; state root {} verified at the pivot {pivot}",
        c.trie_nodes, anchor.state_root
    );
    // An account above 2^256 is tracked by address by the executor. snap/1 gives only its hash, so
    // its preimage (a handful of accounts at most) is asked of an xOne peer over xpre/1, checked by
    // its hash, or found among the genesis accounts; no other account needs one.
    let mut wide_pre = Vec::new();
    for h in wide {
        // an xOne peer knows it, or it is a genesis account (the funded accounts of the XDC networks
        // are the ones above 2^256), checked by its hash either way
        let p = wide_preimage(d, h)
            .or_else(|| {
                genesis
                    .alloc
                    .keys()
                    .find(|a| keccak256(a) == h)
                    .map(|a| Bytes::copy_from_slice(a.as_slice()))
            })
            .or_else(|| {
                // the archive RPC of --snap.gap-rpc knows it (checked by its hash)
                let url = cfg.sync.snap_gap_rpc.as_ref()?;
                let mut rpc = gaprpc::Rpc::new(url, pivot).ok()?;
                rpc.preimage(h)
                    .ok()
                    .map(|a| Bytes::copy_from_slice(a.as_slice()))
            })
            .ok_or(SnapSyncError::WideAddress(h))?;
        wide_pre.push((h, p));
    }
    eprintln!(
        "snap: state root {got} rebuilt from {} hashed accounts and {} hashed slots; no preimages needed",
        if rows_in_tables { "the".to_string() } else { rows.accounts.len().to_string() },
        if rows_in_tables { "stored".to_string() } else { rows.storage.len().to_string() }
    );

    // ---- install
    side.codes = codes;
    let install = Install {
        anchor: Bytes::from(anchor.encode()),
        headers: win,
        records,
        state: rows,
        preimages: wide_pre,
        retention: RETENTION,
        rows_in_tables,
    };
    crate::snapshot::install(&dir, &mut store, pivot, anchor.hash, td, &side, install)
        .map_err(|e| SnapSyncError::Io(e.to_string()))?;
    let record = format!(
        "pivot {pivot} {}\nstate_root {}\nswitch {} {}\npin {pin_source}\nsynced_at {now}\n",
        anchor.hash, anchor.state_root, pv.switch_number, pv.switch_hash
    );
    std::fs::write(dir.join("snap-pivot.txt"), record)
        .map_err(|e| SnapSyncError::Io(e.to_string()))?;
    eprintln!(
        "snap: installed pivot {pivot} as an anchored datadir in {} s total ({} MB over snap/1); executing from {}",
        t0.elapsed().as_secs(),
        d.n.snap_bytes / 1_000_000,
        pivot + 1
    );
    Ok(())
}

/// Source peers tried for one pivot.
const PIVOT_TRIES: u32 = 6;
/// Most re-pivots in one sync: each is a fresh pivot for a download the peers' state window outran.
const MAX_REPIVOTS: u32 = 64;

/// Downloads the state of a pivot with `run`; on `SyncError::Unavailable` asks `pivot` for a fresh one
/// and runs again over the same store (`StateStore::set_root`), up to [`MAX_REPIVOTS`] times.
fn repivot<C, T>(
    c: &mut C,
    pivot: impl FnMut(&mut C) -> R<(T, B256)>,
    run: impl FnMut(&mut StateStore, &mut C) -> Result<(), SyncError>,
) -> R<(T, StateStore)> {
    repivot_with(c, pivot, |root| Ok(StateStore::new(root)), run)
}

/// [`repivot`] over any [`Backend`]: `mk` opens it at the first pivot's root (a store on disk resumes
/// the cursors of an earlier run there).
fn repivot_with<C, T, B: Backend>(
    c: &mut C,
    mut pivot: impl FnMut(&mut C) -> R<(T, B256)>,
    mk: impl FnOnce(B256) -> R<B>,
    mut run: impl FnMut(&mut B, &mut C) -> Result<(), SyncError>,
) -> R<(T, B)> {
    let mut t = Instant::now();
    let (mut p, root) = pivot(c)?;
    let mut setup = t.elapsed().as_secs();
    let mut st = mk(root)?;
    let mut moves = 0;
    loop {
        t = Instant::now();
        match run(&mut st, c) {
            Ok(()) => return Ok((p, st)),
            Err(SyncError::Unavailable) if moves < MAX_REPIVOTS => {
                moves += 1;
                let down = t.elapsed().as_secs();
                t = Instant::now();
                let (np, root) = pivot(c)?;
                let next = t.elapsed().as_secs();
                eprintln!(
                    "snap: no peer serves root {}: pivot moved to root {root} (move {moves}); kept {}; last pivot: {setup} s to set up, {down} s of download; next: {next} s to set up",
                    st.progress().root,
                    st.summary(),
                );
                setup = next;
                p = np;
                st.set_root(root)
                    .map_err(|e| SnapSyncError::Io(e.to_string()))?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Follows the chain from `head` to the tip of `src`: the headers after it, each linked to its parent.
fn advance(d: &mut Drv<'_>, src: PeerId, mut head: Header) -> R<(Header, B256)> {
    loop {
        let more = d.headers_from(src, Origin::Number(head.number + 1), HEADERS)?;
        for x in &more {
            if x.number != head.number + 1 || x.parent_hash != head.hash() {
                return Err(bad(&src, format!("header {} does not link", x.number)));
            }
            head = x.clone();
        }
        if more.len() < HEADERS as usize {
            let h = head.hash();
            return Ok((head, h));
        }
    }
}

/// The total difficulty at `pivot`, from a peer's Status (`td` at block `at`) and the linked headers
/// between the two: `td(n) = td(n - 1) + difficulty(n)`, which is what geth-XDC puts in its Status
/// (`xdcHandshakeTD`, `eth/protocols/eth/handshake.go:87-104`: the stored TD of the head). `None`
/// when a header between them is not held, or the Status td saturated (`Peers` ranks with a u64).
fn td_at(headers: &BTreeMap<u64, Header>, (at, td): (u64, u64), pivot: u64) -> Option<U256> {
    if td == u64::MAX {
        return None;
    }
    let mut t = U256::from(td);
    for n in pivot.min(at) + 1..=pivot.max(at) {
        let d = headers.get(&n)?.difficulty;
        t = if pivot < at { t.checked_sub(d)? } else { t + d };
    }
    Some(t)
}

/// What one pivot needs before its state is downloaded: the verified anchor and the window and gap
/// pools below it.
struct Pivoted {
    anchor: Anchor,
    /// The total difficulty at the pivot; 0 when the source's Status does not give it.
    td: U256,
    pin_source: String,
    win: Vec<Header>,
    records: Vec<Record>,
    side: Side,
    now: u64,
}

/// The head of the source peer, the pivot `head - 64` (lowered to the first block whose anchor
/// verifies), its window of headers, bodies and receipts, and the candidate pool of each gap block.
fn pivot_window(
    d: &mut Drv<'_>,
    cfg: &Config,
    spec: &ChainSpec,
    pins: &[(u64, B256)],
) -> R<Pivoted> {
    // a peer that answered "no data" at the last pivot may serve the next one
    d.faults.clear();
    // a source that fails (a stranger that does not serve its own head) is replaced by the next
    for _ in 1..PIVOT_TRIES {
        match pivot_once(d, cfg, spec, pins) {
            Err(e @ SnapSyncError::Peer { .. }) => {
                eprintln!("snap: {e}; trying another source");
                if let Ok(p) = d.src() {
                    d.fault(&p);
                }
            }
            r => return r,
        }
    }
    pivot_once(d, cfg, spec, pins)
}

fn pivot_once(d: &mut Drv<'_>, cfg: &Config, spec: &ChainSpec, pins: &[(u64, B256)]) -> R<Pivoted> {
    // ---- the head and the pivot
    let src = d.src()?;
    let (head_hash, rank) = d
        .known
        .get(&src)
        .map(|p| (p.head, p.rank))
        .ok_or_else(|| bad(&src, "gone"))?;
    let head = d
        .headers_from(src, Origin::Hash(head_hash), 1)?
        .into_iter()
        .next()
        .filter(|h| h.hash() == head_hash)
        .ok_or_else(|| bad(&src, "does not serve its own head"))?;
    let status = (head.number, rank);
    // the Status head is as old as the connection: go on to the peer's tip (a re-pivot, minutes
    // later, would otherwise pick the same stale pivot)
    let (head, head_hash) = advance(d, src, head)?;
    super::snapview::set_head(&head);
    let top = head.number;
    let first_try = top.checked_sub(PIVOT_DEPTH).ok_or(SnapSyncError::NoPivot {
        from: top,
        last: "the chain is shorter than the pivot depth".into(),
    })?;
    // the window of the lowest pivot tried, plus the gap pools, which reach further than the
    // headers on no network but are checked anyway
    let lowest = first_try.saturating_sub(PIVOT_SEARCH);
    let plan_lo = plan_at(spec, lowest).map_err(|e| SnapSyncError::Io(e.to_string()))?;
    let lo = plan_lo
        .needs
        .iter()
        .map(|n| n.first)
        .min()
        .unwrap_or(lowest)
        .max(1);
    let mut headers: BTreeMap<u64, Header> = BTreeMap::new();
    let mut n = lo;
    while n <= top {
        let amount = HEADERS.min(top - n + 1);
        let batch = d.headers_from(src, Origin::Number(n), amount)?;
        if batch.is_empty() {
            return Err(bad(&src, format!("no headers from {n}")));
        }
        for x in batch {
            if x.number != n {
                return Err(bad(
                    &src,
                    format!("header {} where {n} was asked", x.number),
                ));
            }
            if let Some(p) = headers.get(&(n - 1)) {
                if x.parent_hash != p.hash() {
                    return Err(bad(&src, format!("header {n} does not link")));
                }
            }
            headers.insert(n, x);
            n += 1;
        }
    }
    if headers.get(&top).map(Header::hash) != Some(head_hash) {
        return Err(bad(&src, "the header run does not end at its head"));
    }
    // a pivot on the abandoned branch is refused before any state is asked for
    let required = cfg
        .required_blocks_of_chain(spec.chain_id)
        .map_err(|e| SnapSyncError::Io(e.to_string()))?;
    check_required_run(&required, top, &headers, |n| {
        Ok(d.headers_from(src, Origin::Number(n), 1)?
            .first()
            .filter(|h| h.number == n)
            .map(Header::hash))
    })?;
    eprintln!(
        "snap: head {top} {head_hash} from peer {}; headers {lo}..={top} downloaded and linked ({} headers)",
        short(&src),
        headers.len()
    );
    let get = |n: u64| headers.get(&n).cloned();
    let now = now();
    let mut last = String::new();
    let mut chosen: Option<(Anchor, String)> = None;
    for p in (lowest..=first_try).rev() {
        let a = match build_anchor(spec, &get, top, p, now, 3600) {
            Ok(a) => a,
            Err(e) => {
                last = e.to_string();
                continue;
            }
        };
        if spec.xdpos.v2.as_ref().is_none_or(|v| p <= v.switch_block) {
            return Err(SnapSyncError::V1(p));
        }
        let pv = &a.provenance;
        let (pins_now, source) = if pins.is_empty() {
            witness(d, pv.switch_number, pv.switch_hash)?
        } else {
            (pins.to_vec(), "operator pins file".to_owned())
        };
        let ctx = Context {
            now,
            pins: &pins_now,
            max_age_cap: u64::MAX,
        };
        match verify_anchor(spec, &a, &ctx) {
            Ok(()) => {
                chosen = Some((a, source));
                break;
            }
            Err(e @ AnchorError::NotPinned) => return Err(SnapSyncError::Anchor(p, e)),
            Err(e) => last = e.to_string(),
        }
    }
    let Some((anchor, pin_source)) = chosen else {
        return Err(SnapSyncError::NoPivot {
            from: first_try,
            last,
        });
    };
    let pivot = anchor.number;
    let td = td_at(&headers, status, pivot).unwrap_or(U256::ZERO);
    eprintln!(
        "snap: total difficulty {td} at the pivot, from the Status of peer {} (td {} at block {}) and the linked headers",
        short(&src),
        status.1,
        status.0
    );
    let pv = &anchor.provenance;
    eprintln!(
        "snap: pivot block {pivot} {} state root {} (head {top} - {}); anchor verified: switch {} {} with {} masternodes certifies it by the 3-chain to {}; pin: {pin_source}",
        anchor.hash,
        anchor.state_root,
        top - pivot,
        pv.switch_number,
        pv.switch_hash,
        pv.validators.len(),
        pivot + 3
    );

    // ---- the window: headers, bodies, receipts, signs
    let plan = plan_at(spec, pivot).map_err(|e| SnapSyncError::Io(e.to_string()))?;
    let floor = |c: Class| plan.of(c).next().map(|n| n.first);
    let headers_first = floor(Class::Header).unwrap_or(pivot);
    let records_first = [Class::Body, Class::Receipt]
        .into_iter()
        .filter_map(floor)
        .min()
        .unwrap_or(pivot)
        .min(pivot)
        .max(headers_first);
    let win: Vec<Header> = (headers_first..=pivot)
        .map(|n| {
            headers.get(&n).cloned().ok_or(SnapSyncError::Block {
                number: n,
                why: "header not downloaded".into(),
            })
        })
        .collect::<R<_>>()?;
    let verifier = BodyVerifier::new(spec);
    let mut records = Vec::new();
    let mut side = Side::default();
    let mut todo: Vec<&Header> = win.iter().filter(|h| h.number >= records_first).collect();
    while !todo.is_empty() {
        let src = d.src()?;
        let chunk: Vec<&Header> = todo.iter().take(BODIES).copied().collect();
        let hashes: Vec<B256> = chunk.iter().map(|h| h.hash()).collect();
        let bodies = match d.eth(src, |id| Eth::GetBlockBodies {
            id,
            hashes: hashes.clone(),
        })? {
            Eth::BlockBodies { bodies, .. } => bodies,
            _ => vec![],
        };
        let receipts = match d.eth(src, |id| Eth::GetReceipts {
            id,
            first_index: 0,
            hashes: hashes.clone(),
        })? {
            Eth::Receipts { per_block, .. } => per_block,
            _ => vec![],
        };
        let got = bodies.len().min(receipts.len());
        if got == 0 {
            d.fault(&src);
            return Err(bad(&src, "no bodies or receipts for the window"));
        }
        for ((header, b), r) in chunk.iter().zip(bodies).zip(receipts).take(got) {
            let number = header.number;
            let berr = |why: String| SnapSyncError::Block { number, why };
            let body = to_body(b).0;
            let receipts = receipts_of(&r).map_err(|e| berr(e.to_string()))?;
            verifier
                .verify_body(header, &body)
                .map_err(|e| berr(e.to_string()))?;
            verifier
                .verify_receipts(header, body.transactions.len(), &receipts)
                .map_err(|e| berr(e.to_string()))?;
            let signs = signs_of(spec.chain_id, number, &body, &receipts)?;
            if !signs.is_empty() {
                side.signs.insert(number, signs);
            }
            records.push(Record {
                block: Block::new((*header).clone(), body),
                receipts,
                diff: Default::default(),
                aux: Bytes::new(),
            });
        }
        todo.drain(..got);
    }
    eprintln!(
        "snap: window: headers {headers_first}..={pivot}, bodies and receipts {records_first}..={pivot} ({} blocks, {} with sign transactions), each checked against its header",
        records.len(),
        side.signs.len()
    );

    // ---- the gap pools
    let scfg = client::Config::default();
    let k88 = keccak256(validator_0x88::ADDRESS);
    for need in plan.of(Class::GapSnapshot) {
        let g = need.first;
        let gerr = |why: String| SnapSyncError::Gap { number: g, why };
        if spec.xdpos.v2.as_ref().is_none_or(|v| g <= v.switch_block) {
            return Err(SnapSyncError::V1(pivot));
        }
        let root = headers
            .get(&g)
            .map(|h| h.state_root)
            .ok_or_else(|| gerr("header not downloaded".into()))?;
        let pool = gap_pool(d, cfg, &scfg, root, g, k88)?;
        side.gaps.insert(g, pool);
    }
    Ok(Pivoted {
        anchor,
        td,
        pin_source,
        win,
        records,
        side,
        now,
    })
}

/// The address behind `hash` from an xOne peer (xpre/1), kept only if it hashes to `hash`.
fn wide_preimage(d: &mut Drv<'_>, hash: B256) -> Option<Bytes> {
    for _ in 0..3 {
        let peer = d.pick(true)?;
        d.next_id += 1;
        let id = d.next_id;
        let req = preimage::Message::Get(GetPreimages {
            id,
            hashes: vec![hash],
        });
        let Ok((code, body)) = d.sub(peer, Sub::Xpre, req.code(), req.encode(), id) else {
            d.fault(&peer);
            continue;
        };
        if let Ok(preimage::Message::Answer(a)) = preimage::Message::decode(code, &body) {
            if let Some(p) = a
                .preimages
                .into_iter()
                .find(|p| p.len() == 20 && keccak256(p) == hash)
            {
                return Some(p);
            }
        }
        d.fault(&peer);
    }
    None
}

/// The pool of gap block `g` from its 0x88 state, in this order: an xOne peer over snap/1 (its window
/// keeps the state), else the archive RPC of `--snap.gap-rpc` with every proof verified against the
/// header's state root, else a typed error that names both. Never an empty pool.
fn gap_pool(
    d: &mut Drv<'_>,
    cfg: &Config,
    scfg: &client::Config,
    root: B256,
    g: u64,
    k88: B256,
) -> R<Vec<Address>> {
    let gerr = |why: String| SnapSyncError::Gap { number: g, why };
    if d.pick(true).is_some() {
        match client::account_at(&mut Src(d, true), scfg, root, k88) {
            Ok(Some((account, slots))) => {
                let n = slots.len();
                let pool = gap_snapshot::build(&One { account, slots }, g)
                    .map_err(|e| gerr(e.to_string()))?;
                eprintln!(
                    "snap: gap block {g} root {root}: 0x88 account and {n} slots proven over snap/1 from an xOne peer, {} candidates",
                    pool.candidates.len()
                );
                return Ok(pool.candidates);
            }
            Ok(None) => {
                eprintln!("snap: gap block {g}: the xOne peer has no 0x88 account at {root}")
            }
            Err(e) => eprintln!("snap: gap block {g}: the xOne peer did not serve it: {e}"),
        }
    }
    let Some(url) = &cfg.sync.snap_gap_rpc else {
        return Err(SnapSyncError::GapSource { number: g });
    };
    let mut rpc = gaprpc::Rpc::new(url, g).map_err(|e| gerr(format!("--snap.gap-rpc: {e}")))?;
    let pool = gaprpc::pool(&mut rpc, root, g).map_err(|e| gerr(format!("--snap.gap-rpc: {e}")))?;
    eprintln!(
        "snap: gap block {g} root {root}: 0x88 pool of {} candidates, {} eth_getProof requests, every proof verified against the header's state root",
        pool.len(),
        rpc.requests
    );
    Ok(pool)
}

/// The pin of the anchor (geth's trust in its peers' header chain): at least two peers serve the
/// switch header `hash` and more peers serve it than serve any other hash. Peers on another chain
/// (a stale release that forked off) are outvoted and reported, not fatal.
/// Every required height at or below `top` must carry its hash in the chain that ends at `top`:
/// from `headers` where they reach, else as the source serves it (`fetch`; `None` is a source that
/// does not have a block it announced the head above).
fn check_required_run(
    required: &[(u64, B256)],
    top: u64,
    headers: &BTreeMap<u64, Header>,
    mut fetch: impl FnMut(u64) -> R<Option<B256>>,
) -> R<()> {
    for &(n, want) in required.iter().filter(|(n, _)| *n <= top) {
        let got = match headers.get(&n) {
            Some(h) => h.hash(),
            None => fetch(n)?.ok_or(SnapSyncError::Block {
                number: n,
                why: "the source does not serve a required block below its head".into(),
            })?,
        };
        xone_chainspec::check_required(&[(n, want)], n, got)?;
    }
    Ok(())
}

fn witness(d: &mut Drv<'_>, number: u64, hash: B256) -> R<(Vec<(u64, B256)>, String)> {
    // peers that connect late (the wait for the first snap peer returns as soon as two are there,
    // and one may be a stranger) get a minute to join the vote
    let give_up = Instant::now() + PEER_WAIT / 2;
    loop {
        let mut votes = Vec::new();
        let ids: Vec<PeerId> = d.known.keys().copied().collect();
        for p in ids {
            let Ok(h) = d.headers_from(p, Origin::Number(number), 1) else {
                continue;
            };
            if let Some(x) = h.first().map(Header::hash) {
                votes.push((short(&p), x));
            }
        }
        match tally(number, hash, &votes) {
            Err(SnapSyncError::Witnesses { .. }) if Instant::now() < give_up => {
                let pause = Instant::now() + Duration::from_secs(5);
                while d.next(pause)?.is_some() {}
            }
            r => {
                let agree = r?;
                return Ok((
                    vec![(number, hash)],
                    format!("peers {} serve switch {number} {hash}", agree.join(",")),
                ));
            }
        }
    }
}

/// The peers of `votes` (peer, hash served) that serve `hash`, if they are at least two and outnumber
/// the peers that serve another hash.
fn tally(number: u64, hash: B256, votes: &[(String, B256)]) -> R<Vec<String>> {
    const NEED: usize = 2;
    let agree: Vec<String> = votes
        .iter()
        .filter(|(_, x)| *x == hash)
        .map(|(p, _)| p.clone())
        .collect();
    for (p, x) in votes.iter().filter(|(_, x)| *x != hash) {
        eprintln!("snap: peer {p} serves {x} for switch {number}, not {hash}; outvoted or refused");
    }
    let against = votes.len() - agree.len();
    if agree.len() < NEED {
        return Err(SnapSyncError::Witnesses {
            number,
            hash,
            have: agree.len(),
            need: NEED,
        });
    }
    if agree.len() <= against {
        let theirs = votes.iter().map(|(_, x)| *x).find(|x| *x != hash);
        return Err(SnapSyncError::Disputed {
            number,
            peer: format!("{against} peer(s)"),
            ours: hash,
            theirs: theirs.unwrap_or_default(),
        });
    }
    Ok(agree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_rlp::Encodable;

    /// #119: the pivot's total difficulty is the Status td less the difficulty above the pivot
    /// (or plus the difficulty up to it, for a Status older than the pivot).
    #[test]
    fn the_pivot_td_is_the_status_td_through_the_linked_headers() {
        let headers: BTreeMap<u64, Header> = (10..=20)
            .map(|n| {
                let h = Header {
                    number: n,
                    difficulty: U256::from(if n < 15 { 3 } else { 1 }),
                    ..Default::default()
                };
                (n, h)
            })
            .collect();
        // a full node at 20 has td 100: 12..=20 add 3 * 3 + 6 * 1 = 15 above 11
        assert_eq!(td_at(&headers, (20, 100), 11), Some(U256::from(85)));
        assert_eq!(td_at(&headers, (11, 85), 20), Some(U256::from(100)));
        assert_eq!(td_at(&headers, (20, 100), 20), Some(U256::from(100)));
        // a header that is not held, a saturated td
        assert_eq!(td_at(&headers, (20, 100), 5), None);
        assert_eq!(td_at(&headers, (20, u64::MAX), 11), None);
    }

    /// What `chain.rs` serves for `GetReceipts` is what the snap client reads back: a legacy
    /// receipt as its list, a typed one inside an RLP string.
    #[test]
    fn receipts_round_trip_through_the_wire() {
        let legacy = Receipt {
            tx_type: 0,
            post_state_or_status: vec![1u8].into(),
            cumulative_gas_used: 21_000,
            logs: vec![],
        };
        let typed = Receipt {
            tx_type: 2,
            post_state_or_status: Bytes::new(),
            cumulative_gas_used: 42_000,
            logs: vec![],
        };
        let enc: Vec<Bytes> = [legacy, typed]
            .iter()
            .map(|r| Bytes::from(r.encode_consensus()))
            .collect();
        let wire = super::super::chain::receipts_wire(&enc);
        let raw: Raw =
            alloy_rlp::decode_exact(alloy_rlp::encode(&wire)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(receipts_of(&raw).unwrap_or_else(|e| panic!("{e}")), enc);
        assert!(receipts_of(&Raw(vec![0x80].into())).is_err());
    }

    /// geth-XDC answers GetReceipts with eth/69's receipt (no bloom); the consensus encoding that
    /// the receipts root is computed over comes out the same as xOne's own form.
    #[test]
    fn an_eth69_receipt_becomes_the_consensus_receipt() {
        let log = alloy_primitives::Log::new_unchecked(
            alloy_primitives::Address::repeat_byte(7),
            vec![B256::repeat_byte(1)],
            Bytes::from_static(b"d"),
        );
        for (tx_type, status) in [(0u8, vec![1u8]), (2, vec![]), (0, vec![0x11; 32])] {
            let want = Receipt {
                tx_type,
                post_state_or_status: Bytes::from(status.clone()),
                cumulative_gas_used: 63_000,
                logs: vec![log.clone()],
            };
            let mut inner = Vec::new();
            tx_type.encode(&mut inner);
            Bytes::from(status).encode(&mut inner);
            63_000u64.encode(&mut inner);
            vec![log.clone()].encode(&mut inner);
            let mut item = Vec::new();
            alloy_rlp::Header {
                list: true,
                payload_length: inner.len(),
            }
            .encode(&mut item);
            item.extend_from_slice(&inner);
            let got = receipt_of(&Raw(item.into())).unwrap();
            assert_eq!(got.to_vec(), want.encode_consensus());
        }
    }

    #[test]
    fn the_pin_is_the_majority_of_at_least_two() {
        let (a, b) = (B256::repeat_byte(1), B256::repeat_byte(2));
        let v = |x: &[(&str, B256)]| -> Vec<(String, B256)> {
            x.iter().map(|(p, h)| ((*p).to_owned(), *h)).collect()
        };
        assert_eq!(
            tally(900, a, &v(&[("p1", a), ("p2", a), ("p3", b)])).unwrap(),
            ["p1", "p2"]
        );
        // a lone witness is not enough, and a tie is a dispute
        assert!(matches!(
            tally(900, a, &v(&[("p1", a), ("p2", b)])),
            Err(SnapSyncError::Witnesses { have: 1, .. })
        ));
        assert!(matches!(
            tally(900, a, &v(&[("p1", a), ("p2", a), ("p3", b), ("p4", b)])),
            Err(SnapSyncError::Disputed { .. })
        ));
        // the peers that serve another hash win: our hash is the minority chain
        assert!(tally(900, a, &v(&[("p1", a), ("p2", b), ("p3", b)])).is_err());
    }

    /// An `Unavailable` download takes a fresh pivot and continues over the same store; any other error
    /// stops it, and the moves are bounded.
    #[test]
    fn a_pivot_chain_through_the_wrong_required_hash_is_refused() {
        let h = |n: u64, salt: u8| Header {
            number: n,
            extra_data: vec![salt].into(),
            ..Header::default()
        };
        let run: BTreeMap<u64, Header> = (10..=20).map(|n| (n, h(n, 1))).collect();
        let none = |_| -> R<Option<B256>> { panic!("held in the run") };
        // in the run: the right hash passes, another is a typed error
        assert!(check_required_run(&[(15, h(15, 1).hash())], 20, &run, none).is_ok());
        let e = check_required_run(&[(15, h(15, 2).hash())], 20, &run, none).unwrap_err();
        assert!(
            matches!(&e, SnapSyncError::RequiredBlock(m) if (m.number, m.got) == (15, h(15, 1).hash())),
            "{e}"
        );
        // below the run: asked of the source
        let asked = |n: u64| -> R<Option<B256>> { Ok(Some(h(n, 1).hash())) };
        assert!(check_required_run(&[(5, h(5, 1).hash())], 20, &run, asked).is_ok());
        let e = check_required_run(&[(5, h(5, 2).hash())], 20, &run, asked).unwrap_err();
        assert!(matches!(e, SnapSyncError::RequiredBlock(_)), "{e}");
        // above the head: not applicable; a source without the block: refused
        assert!(check_required_run(&[(99, B256::ZERO)], 20, &run, none).is_ok());
        let gone = |_| -> R<Option<B256>> { Ok(None) };
        assert!(check_required_run(&[(5, B256::ZERO)], 20, &run, gone).is_err());
    }

    #[test]
    fn unavailable_moves_the_pivot_and_keeps_the_store() {
        let root = |n: u8| B256::repeat_byte(n);
        let mut asked = vec![];
        let (p, st) = repivot(
            &mut 0u8,
            |n| {
                *n += 1;
                Ok((*n, root(*n)))
            },
            |st, _| {
                asked.push(st.progress.root);
                if st.progress.root == root(3) {
                    return Ok(());
                }
                // the data of this run stays through the move
                st.accounts.insert(st.progress.root, Account::default());
                Err(SyncError::Unavailable)
            },
        )
        .unwrap();
        assert_eq!((p, st.progress.root), (3, root(3)));
        assert_eq!(asked, [root(1), root(2), root(3)]);
        assert_eq!(st.accounts.len(), 2);
        assert!(st.progress.stale);

        let e = repivot(
            &mut (),
            |_| Ok(((), root(1))),
            |_, _| Err(SyncError::Unavailable),
        );
        assert!(matches!(
            e,
            Err(SnapSyncError::State(SyncError::Unavailable))
        ));
        let mut n = 0;
        let e = repivot(
            &mut (),
            |_| Ok(((), root(1))),
            |_, _| {
                n += 1;
                Err(SyncError::Peer(xone_snap::client::PeerError::Timeout))
            },
        );
        assert!(e.is_err() && n == 1);
    }
}
