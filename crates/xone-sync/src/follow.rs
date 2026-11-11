//! Live follow at the tip: a pure driver over a [`PeerSource`]. Pushed blocks and announcements
//! are imported through `Chain::insert`; a gap is filled with header and body requests. Specification:
//! `spec/sync/follow.md`.
//!
//! * A peer's TD (`rank`) orders the peers and nothing else (geth `rankedPeers`,
//!   `eth/sync_xdc.go:6330-6376`; recreate guide 11: TD is not a block number). It never says
//!   "behind" and never says "caught up".
//! * Caught up is hash equality: some peer's head hash is our head hash (`:1759-1769`).
//! * At the tip is that, or a fresh head: head age within [`at_tip_freshness_secs`] (`:571-589`,
//!   `:756-765`), three ticks in a row (`:766`).
//! * A peer that serves nothing for the block we need is skipped for that block and the next peer
//!   in rank order is asked (`:2574-2584`). If none serves it the step reports the stall; it does
//!   not wait.
//!
//! No clock, no I/O, no threads: `now` is an argument and the source does the requests.

use crate::{
    chain::{Chain, ChainRead, HeaderCheck, InsertError, Outcome},
    executor::BlockExecutor,
    store::{Block, Store},
};
use alloy_primitives::B256;
use std::{collections::VecDeque, fmt::Debug};
use xone_chainspec::ChainSpec;
use xone_consensus::verify::Body;
use xone_primitives::Header;

/// geth `xdpos2MaxHeaderFetch` (`eth/sync_xdc.go:115`): legacy peers answer a longer header request
/// empty.
pub const MAX_HEADER_FETCH: usize = 192;
/// Blocks per gap request: ADR-007 Q9 and geth `MaxBlockFetch` (`eth/downloader/downloader.go:45`),
/// which is below the header cap, so one request pair serves one batch.
pub const GAP_BATCH: usize = 128;
/// ADR-007 Q4: pushed blocks awaiting import. A dropped block is a gap, filled by request.
pub const Q4_BLOCKS: usize = 16;
/// Announcements kept (ADR-007 Q3 allows 16 per peer; this is the total).
pub const ANNOUNCES: usize = 64;
/// geth `tipCatchAtPeerTicks >= 3` (`eth/sync_xdc.go:766`).
pub const TIP_TICKS: u32 = 3;
/// Ancestors fetched by hash to connect a pushed block whose parent is unknown and which is not
/// above the head.
pub const FORK_DEPTH: usize = 32;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("peer: {0}")]
pub struct PeerError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo<P> {
    pub peer: P,
    /// The head hash of the peer's status.
    pub head: B256,
    /// Its TD, saturated to `u64`. Ranking only.
    pub rank: u64,
}

#[derive(Debug, Clone)]
pub enum Push<P> {
    /// `NewBlock`: the block and the announcer's TD.
    NewBlock {
        peer: P,
        block: Box<Block>,
        rank: u64,
    },
    /// One entry of `NewBlockHashes`.
    Announce { peer: P, hash: B256, number: u64 },
}

/// What the driver needs of the network. Every answer is untrusted: the driver checks linkage and
/// body against header, and `Chain::insert` checks the rest.
pub trait PeerSource {
    type Peer: Clone + PartialEq + Debug;
    /// Connected peers with their status head and rank.
    fn peers(&mut self) -> Vec<PeerInfo<Self::Peer>>;
    /// Pushed messages since the last call.
    fn poll(&mut self) -> Vec<Push<Self::Peer>>;
    /// `GetBlockHeaders` by number, ascending, at most `max`.
    fn headers_by_number(
        &mut self,
        peer: &Self::Peer,
        from: u64,
        max: usize,
    ) -> Result<Vec<Header>, PeerError>;
    /// `GetBlockHeaders` by hash, one header.
    fn header_by_hash(
        &mut self,
        peer: &Self::Peer,
        hash: B256,
    ) -> Result<Option<Header>, PeerError>;
    /// `GetBlockBodies` for `headers`; the answer may be a prefix.
    fn bodies(&mut self, peer: &Self::Peer, headers: &[Header]) -> Result<Vec<Body>, PeerError>;
}

/// geth `atTipFreshnessWindow` (`eth/sync_xdc.go:571-589`): 30 s, or on a V2 chain the timeout
/// budget `TimeoutPeriod * TimeoutSyncThreshold + MinePeriod` when that is larger.
pub fn at_tip_freshness_secs(spec: &ChainSpec) -> u64 {
    const FLOOR: u64 = 30;
    let Some(c) = spec
        .xdpos
        .v2
        .as_ref()
        .and_then(|v| v.configs.values().next_back())
        .filter(|c| c.timeout_period > 0)
    else {
        return FLOOR;
    };
    (c.timeout_period * c.timeout_sync_threshold.max(1) + c.mine_period).max(FLOOR)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub head: (u64, B256),
    /// Some peer's head hash is our head hash, and we are within one epoch of the highest block seen.
    pub caught_up: bool,
    /// The head is no older than the freshness window.
    pub head_fresh: bool,
    /// `caught_up` or `head_fresh` for [`TIP_TICKS`] steps in a row.
    pub at_tip: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Blocks that became part of the chain (any outcome but `Known`).
    pub imported: u64,
    /// Blocks a peer served that the chain refused.
    pub rejected: u64,
    /// The block we needed and no peer served.
    pub stalled_at: Option<u64>,
    /// The last error `Chain::insert` gave for a block a peer served, for the operator's log.
    pub last_rejection: Option<String>,
    pub status: Status,
}

pub struct Follower<P> {
    queue: VecDeque<(P, Block)>,
    announced: VecDeque<(P, B256, u64)>,
    /// Highest block number heard of.
    known_tip: u64,
    ticks: u32,
    fresh_secs: u64,
    epoch: u64,
    /// Pushed blocks and announcements dropped by the bounds.
    pub dropped: u64,
}

impl<P: Clone + PartialEq + Debug> Follower<P> {
    pub fn new(spec: &ChainSpec) -> Self {
        Self {
            queue: VecDeque::new(),
            announced: VecDeque::new(),
            known_tip: 0,
            ticks: 0,
            fresh_secs: at_tip_freshness_secs(spec),
            epoch: spec.xdpos.epoch,
            dropped: 0,
        }
    }

    /// One pass: ingest pushes, import what they carry, then look at the peers' heads. Only a store
    /// failure is an `Err`.
    pub fn step<S: Store, E: BlockExecutor, H: HeaderCheck>(
        &mut self,
        chain: &Chain<S, E, H>,
        src: &mut impl PeerSource<Peer = P>,
        now: u64,
    ) -> Result<Report, InsertError> {
        let mut rep = Report {
            imported: 0,
            rejected: 0,
            stalled_at: None,
            last_rejection: None,
            status: self.status(chain, &[], now, false),
        };
        for p in src.poll() {
            match p {
                Push::NewBlock { peer, block, .. } => {
                    self.known_tip = self.known_tip.max(block.number());
                    if self.queue.len() == Q4_BLOCKS {
                        self.queue.pop_front(); // DropOldest: the gap is refetched
                        self.dropped += 1;
                    }
                    self.queue.push_back((peer, *block));
                }
                Push::Announce { peer, hash, number } => {
                    self.known_tip = self.known_tip.max(number);
                    if self.announced.len() == ANNOUNCES {
                        self.announced.pop_front();
                        self.dropped += 1;
                    }
                    self.announced.push_back((peer, hash, number));
                }
            }
        }
        while let Some((peer, hash, _)) = self.announced.pop_front() {
            if chain.reader().header(hash).is_some() {
                continue;
            }
            if let Some(block) = fetch_block(src, &peer, hash) {
                self.queue.push_back((peer, block));
            }
        }
        let mut pushed: Vec<_> = self.queue.drain(..).collect();
        pushed.sort_by_key(|(_, b)| b.number());
        for (peer, block) in pushed {
            self.import(chain, src, &peer, block, now, &mut rep)?;
        }
        // The tip poll: a peer whose head we do not hold is ahead of us, or on another branch.
        let peers = ranked(src.peers());
        for p in &peers {
            if chain.reader().header(p.head).is_some() {
                continue;
            }
            let Ok(Some(h)) = src
                .header_by_hash(&p.peer, p.head)
                .map(|h| h.filter(|h| h.hash() == p.head))
            else {
                continue; // it cannot show its own head: ask the next peer in rank order
            };
            let Some(h) = Some(h) else { continue };
            self.known_tip = self.known_tip.max(h.number);
            let head = chain.head().0;
            if h.number > head {
                if let Err(at) = fill(
                    chain,
                    src,
                    &peers,
                    Some(&p.peer),
                    head + 1,
                    h.number,
                    now,
                    &mut rep,
                )? {
                    rep.stalled_at = Some(at);
                }
                break;
            }
        }
        rep.status = self.status(chain, &peers, now, true);
        Ok(rep)
    }

    fn status<S: Store, E: BlockExecutor, H: HeaderCheck>(
        &mut self,
        chain: &Chain<S, E, H>,
        peers: &[PeerInfo<P>],
        now: u64,
        tick: bool,
    ) -> Status {
        let head = chain.head();
        let caught_up =
            peers.iter().any(|p| p.head == head.1) && head.0 + self.epoch >= self.known_tip;
        let head_fresh = chain
            .reader()
            .header(head.1)
            .is_some_and(|h| now >= h.timestamp && now - h.timestamp <= self.fresh_secs);
        if tick {
            self.ticks = if caught_up || head_fresh {
                self.ticks + 1
            } else {
                0
            };
        }
        Status {
            head,
            caught_up,
            head_fresh,
            at_tip: self.ticks >= TIP_TICKS,
        }
    }

    fn import<S: Store, E: BlockExecutor, H: HeaderCheck>(
        &mut self,
        chain: &Chain<S, E, H>,
        src: &mut impl PeerSource<Peer = P>,
        peer: &P,
        block: Block,
        now: u64,
        rep: &mut Report,
    ) -> Result<(), InsertError> {
        match chain.insert(block.clone(), now) {
            Err(InsertError::UnknownParent { .. }) => {}
            r => return settle(r, rep),
        }
        let head = chain.head().0;
        if block.number() > head + 1 {
            // above the head: the missing blocks are head+1..number-1, fetched by number
            let peers = ranked(src.peers());
            if fill(
                chain,
                src,
                &peers,
                Some(peer),
                head + 1,
                block.number() - 1,
                now,
                rep,
            )?
            .is_ok()
            {
                return settle(chain.insert(block, now), rep);
            }
            // they do not connect to our head: the block is on another branch, walk it by hash
        }
        // another branch: walk its ancestors by hash until one is held
        let mut chainup = vec![block];
        while chainup.len() <= FORK_DEPTH {
            let Some(parent) = chainup.last().map(Block::parent) else {
                break;
            };
            if chain.reader().header(parent).is_some() {
                for b in chainup.into_iter().rev() {
                    if settle(chain.insert(b, now), rep).is_err() {
                        break;
                    }
                }
                return Ok(());
            }
            match fetch_block(src, peer, parent) {
                Some(b) => chainup.push(b),
                None => break,
            }
        }
        rep.stalled_at = Some(head + 1); // no ancestor within FORK_DEPTH: nothing connects
        Ok(())
    }
}

/// Counts an insert result. A store failure is the only error that leaves the step.
fn settle(r: Result<Outcome, InsertError>, rep: &mut Report) -> Result<(), InsertError> {
    match r {
        Ok(Outcome::Known) => {}
        Ok(_) => rep.imported += 1,
        Err(e @ InsertError::Store(_)) => return Err(e),
        Err(e) => {
            rep.rejected += 1;
            rep.last_rejection = Some(e.to_string());
        }
    }
    Ok(())
}

fn ranked<P>(mut v: Vec<PeerInfo<P>>) -> Vec<PeerInfo<P>> {
    v.sort_by_key(|p| std::cmp::Reverse(p.rank)); // stable: equal ranks keep the source's order
    v
}

fn fetch_block<P: Clone + PartialEq + Debug>(
    src: &mut impl PeerSource<Peer = P>,
    peer: &P,
    hash: B256,
) -> Option<Block> {
    let h = src.header_by_hash(peer, hash).ok()??;
    if h.hash() != hash {
        return None;
    }
    let body = src.bodies(peer, std::slice::from_ref(&h)).ok()?.pop()?;
    Some(Block::new(h, body))
}

/// Imports blocks `from..=to` served by the peers in rank order, `prefer` first. `Ok(Err(n))` is
/// block `n` that no peer served. A peer is skipped for a block once it served nothing, a wrong
/// header chain, a body that does not match, or a block the chain refused.
#[allow(clippy::too_many_arguments)]
fn fill<P: Clone + PartialEq + Debug, S: Store, E: BlockExecutor, H: HeaderCheck>(
    chain: &Chain<S, E, H>,
    src: &mut impl PeerSource<Peer = P>,
    peers: &[PeerInfo<P>],
    prefer: Option<&P>,
    from: u64,
    to: u64,
    now: u64,
    rep: &mut Report,
) -> Result<Result<(), u64>, InsertError> {
    let mut order: Vec<&P> = peers.iter().map(|p| &p.peer).collect();
    if let Some(p) = prefer {
        order.sort_by_key(|x| *x != p); // stable: the preferred peer first, rank order after
    }
    let mut cur = from;
    while cur <= to {
        let (_, parent) = chain.head();
        let mut served = false;
        for peer in &order {
            let want = usize::try_from(to - cur + 1)
                .unwrap_or(usize::MAX)
                .min(GAP_BATCH.min(MAX_HEADER_FETCH));
            let Ok(hs) = src.headers_by_number(peer, cur, want) else {
                continue;
            };
            let mut link = parent;
            let linked = hs.len() <= want
                && (cur..).zip(&hs).all(|(n, h)| {
                    let ok = h.number == n && h.parent_hash == link;
                    link = h.hash();
                    ok
                });
            if hs.is_empty() || !linked {
                continue;
            }
            let Ok(bodies) = src.bodies(peer, &hs) else {
                continue;
            };
            let mut took = 0;
            for (h, b) in hs.iter().zip(bodies) {
                let before = rep.rejected;
                settle(chain.insert(Block::new(h.clone(), b), now), rep)?;
                if rep.rejected != before {
                    break; // this peer served a block the chain refused
                }
                took += 1;
            }
            if took > 0 {
                cur += took;
                served = true;
                break;
            }
        }
        if !served {
            return Ok(Err(cur));
        }
    }
    Ok(Ok(()))
}
