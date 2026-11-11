//! Propagation as pure functions: what to send to which peer for new pool transactions
//! ([`broadcast`]), what to ask for when a peer announces ([`Fetcher`]) and how much a peer may push
//! at us ([`Intake`]). Messages are values ([`Effect`]); there are no sockets, timers or threads. The
//! caller owns the peers, the clock (`now_ms`) and the wire.
//!
//! Sources, all at the pin: `eth/handler.go:770-824` (`BroadcastTransactions`), `:985-1030` (peer choice),
//! `eth/protocols/eth/broadcast.go`, `peer.go:30-60` and `:360-420`, `eth/fetcher/tx_fetcher.go`;
//! bounds: ADR-007 Q5 and Q6.

use super::PoolTx;
use alloy_primitives::{keccak256, B256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

pub type PeerId = u64;

/// `txMaxBroadcastSize`, `eth/handler.go:68`: a larger transaction is announced, never sent whole.
pub const MAX_TX_BROADCAST_SIZE: u64 = 4096;
/// `maxTxPacketSize`, `eth/protocols/eth/broadcast.go:28`: the size a packet fills before it is sent.
pub const MAX_TX_PACKET_SIZE: u64 = 100 * 1024;
/// `maxKnownTxs`, `peer.go:40`.
pub const MAX_KNOWN_TXS: usize = 32768;
/// `maxQueuedTxs` and `maxQueuedTxAnns`, `peer.go:50`, `:54`.
pub const MAX_QUEUED_TXS: usize = 4096;
/// `maxTxAnnounces`, `tx_fetcher.go:43`: unique hashes one peer may have announced and not delivered.
pub const MAX_TX_ANNOUNCES: usize = 4096;
/// `maxTxRetrievals`, `tx_fetcher.go:53`: hashes in one request.
pub const MAX_TX_RETRIEVALS: usize = 256;
/// `maxTxRetrievalSize`, `tx_fetcher.go:59`: announced bytes in one request.
pub const MAX_TX_RETRIEVAL_SIZE: u64 = 128 * 1024;
/// `txArriveTimeout`, `tx_fetcher.go:71`: how long an announced hash waits for the transaction to arrive by broadcast.
pub const TX_ARRIVE_TIMEOUT_MS: u64 = 500;
/// `txGatherSlack`, `tx_fetcher.go:75`.
pub const TX_GATHER_SLACK_MS: u64 = 100;
/// `txFetchTimeout`, `tx_fetcher.go:89`.
pub const TX_FETCH_TIMEOUT_MS: u64 = 5000;
/// Hashes in one announcement: `maxTxPacketSize` over 32 bytes per hash (`broadcast.go:121-142`).
pub const MAX_ANNOUNCE_HASHES: usize = 3200;

/// ADR-007 Q5: inbound transaction gossip, per peer.
pub const Q5_PEER_TXS: usize = 128;
pub const Q5_PEER_BYTES: u64 = 1024 * 1024;
/// ADR-007 Q6: transaction pool admission (decode and sender recovery), global.
pub const Q6_TXS: usize = 4096;
pub const Q6_BYTES: u64 = 8 * 1024 * 1024;

/// What a peer's protocol version can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// eth/68 and later: `NewPooledTransactionHashes` and `GetPooledTransactions` exist.
    Announce,
    /// eth/100 and eth/101 (`XDPOS2`, `XDPOS2RID`): code 0x08 is the legacy `OrderTxMsg`. Transactions
    /// travel only in `Transactions` (0x02) (`peer.go:380-398`).
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// `Transactions` (0x02): these transactions in full, as one packet.
    Transactions { peer: PeerId, hashes: Vec<B256> },
    /// `NewPooledTransactionHashes`: hash, type and size of each.
    Announce {
        peer: PeerId,
        hashes: Vec<B256>,
        kinds: Vec<u8>,
        sizes: Vec<u32>,
    },
    /// `GetPooledTransactions`.
    Fetch { peer: PeerId, hashes: Vec<B256> },
}

/// The transaction hashes a peer already has or was sent (`knownTxs`), bounded.
#[derive(Debug, Default, Clone)]
pub struct Known {
    set: BTreeSet<B256>,
    order: VecDeque<B256>,
}

impl Known {
    pub fn contains(&self, h: &B256) -> bool {
        self.set.contains(h)
    }
    pub fn add(&mut self, h: B256) {
        if self.set.insert(h) {
            self.order.push_back(h);
            while self.order.len() > MAX_KNOWN_TXS {
                if let Some(old) = self.order.pop_front() {
                    self.set.remove(&old);
                }
            }
        }
    }
    pub fn len(&self) -> usize {
        self.set.len()
    }
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct PeerState {
    pub id: PeerId,
    pub wire: Wire,
    pub known: Known,
}

impl PeerState {
    pub fn new(id: PeerId, wire: Wire) -> Self {
        Self {
            id,
            wire,
            known: Known::default(),
        }
    }
}

/// `choosePeers`, `eth/handler.go:1000-1030`: the `ceil(sqrt(n))` peers with the lowest score receive a
/// transaction whole, the rest an announcement. The score depends on the node (`me`), the peer and the
/// sender, so one sender's transactions go to the same peers. geth keys SipHash with random bytes; the
/// key and the hash are not visible on the wire, so this uses keccak256 over the same inputs.
fn choose_peers(
    peers: &[PeerState],
    sender: &[u8],
    key: &[u8; 16],
    me: PeerId,
) -> BTreeSet<PeerId> {
    let mut scored: Vec<(u64, PeerId)> = peers
        .iter()
        .map(|p| {
            let mut b = Vec::with_capacity(16 + 8 + 8 + sender.len());
            b.extend_from_slice(key);
            b.extend_from_slice(&me.to_be_bytes());
            b.extend_from_slice(&p.id.to_be_bytes());
            b.extend_from_slice(sender);
            let h = keccak256(&b);
            let mut w = [0u8; 8];
            w.copy_from_slice(&h[..8]);
            (u64::from_be_bytes(w), p.id)
        })
        .collect();
    scored.sort();
    let mut n = 0usize;
    while n * n < peers.len() {
        n += 1;
    }
    scored.into_iter().take(n).map(|(_, id)| id).collect()
}

fn size32(t: &PoolTx) -> u32 {
    u32::try_from(t.size).unwrap_or(u32::MAX)
}

/// `BroadcastTransactions` plus the per-peer queues: for each new transaction and each peer that does not
/// know it, either the whole transaction (the chosen peers, small transactions) or its hash. Marks
/// everything as known to the peer, as `AsyncSend*` does. A peer on [`Wire::Full`] gets no announcement
/// (`peer.go:380-398`); the hashes are marked known and nothing is sent, as at the pin.
pub fn broadcast(
    peers: &mut [PeerState],
    txs: &[Arc<PoolTx>],
    key: &[u8; 16],
    me: PeerId,
) -> Vec<Effect> {
    let mut whole: BTreeMap<PeerId, Vec<Arc<PoolTx>>> = BTreeMap::new();
    let mut ann: BTreeMap<PeerId, Vec<Arc<PoolTx>>> = BTreeMap::new();
    for t in txs {
        let direct = if t.size > MAX_TX_BROADCAST_SIZE {
            BTreeSet::new()
        } else {
            choose_peers(peers, t.from().as_slice(), key, me)
        };
        for p in peers.iter() {
            if p.known.contains(&t.hash) {
                continue;
            }
            let dst = if direct.contains(&p.id) {
                &mut whole
            } else {
                &mut ann
            };
            dst.entry(p.id).or_default().push(t.clone());
        }
    }
    let mut out = Vec::new();
    let by_id: BTreeMap<PeerId, usize> = peers.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    for (id, mut list) in whole {
        // Older broadcasts are dropped when the queue is over its cap (`broadcast.go:81`).
        if list.len() > MAX_QUEUED_TXS {
            list.drain(..list.len() - MAX_QUEUED_TXS);
        }
        let Some(&i) = by_id.get(&id) else { continue };
        for t in &list {
            peers[i].known.add(t.hash);
        }
        for packet in packets(&list) {
            out.push(Effect::Transactions {
                peer: id,
                hashes: packet,
            });
        }
    }
    for (id, mut list) in ann {
        if list.len() > MAX_QUEUED_TXS {
            list.drain(..list.len() - MAX_QUEUED_TXS);
        }
        let Some(&i) = by_id.get(&id) else { continue };
        for t in &list {
            peers[i].known.add(t.hash);
        }
        if peers[i].wire == Wire::Full {
            continue;
        }
        for c in list.chunks(MAX_ANNOUNCE_HASHES) {
            out.push(Effect::Announce {
                peer: id,
                hashes: c.iter().map(|t| t.hash).collect(),
                kinds: c.iter().map(|t| t.tx.ty).collect(),
                sizes: c.iter().map(|t| size32(t)).collect(),
            });
        }
    }
    out
}

/// Split into packets: a packet takes transactions until it holds at least `MAX_TX_PACKET_SIZE` bytes,
/// so one may be larger by its last transaction (`broadcast.go:50-56`).
fn packets(txs: &[Arc<PoolTx>]) -> Vec<Vec<B256>> {
    let mut out = Vec::new();
    let mut cur: Vec<B256> = Vec::new();
    let mut size = 0u64;
    for t in txs {
        cur.push(t.hash);
        size += t.size;
        if size >= MAX_TX_PACKET_SIZE {
            out.push(std::mem::take(&mut cur));
            size = 0;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A peer that just connected. On [`Wire::Announce`] the pending hashes are announced (geth
/// `syncTransactions`, `eth/sync.go:26-38`). The pin does nothing for a [`Wire::Full`] peer (its
/// announce is a no-op), so a fresh XDC peer never learns the pool's transactions. xOne sends them whole
/// instead: a sign transaction dropped while a peer was away must reach it (xOneGo `ce79ac6f`).
/// This is a decision of xOne, not parity; it only adds `Transactions` messages.
pub fn sync_new_peer(peer: &mut PeerState, pending: &[Arc<PoolTx>]) -> Vec<Effect> {
    let fresh: Vec<Arc<PoolTx>> = pending
        .iter()
        .filter(|t| !peer.known.contains(&t.hash))
        .cloned()
        .collect();
    for t in &fresh {
        peer.known.add(t.hash);
    }
    match peer.wire {
        Wire::Full => packets(&fresh)
            .into_iter()
            .map(|hashes| Effect::Transactions {
                peer: peer.id,
                hashes,
            })
            .collect(),
        Wire::Announce => fresh
            .chunks(MAX_ANNOUNCE_HASHES)
            .map(|c| Effect::Announce {
                peer: peer.id,
                hashes: c.iter().map(|t| t.hash).collect(),
                kinds: c.iter().map(|t| t.tx.ty).collect(),
                sizes: c.iter().map(|t| size32(t)).collect(),
            })
            .collect(),
    }
}

/// A budget of items and bytes with the overflow action Reject (ADR-007 section 2).
#[derive(Debug, Clone)]
pub struct Budget {
    max_txs: usize,
    max_bytes: u64,
    txs: usize,
    bytes: u64,
}

impl Budget {
    pub fn new(max_txs: usize, max_bytes: u64) -> Self {
        Self {
            max_txs,
            max_bytes,
            txs: 0,
            bytes: 0,
        }
    }
    fn fits(&self, size: u64) -> bool {
        self.txs < self.max_txs && self.bytes.saturating_add(size) <= self.max_bytes
    }
    fn take(&mut self, size: u64) {
        self.txs += 1;
        self.bytes = self.bytes.saturating_add(size);
    }
    fn give(&mut self, size: u64) {
        self.txs = self.txs.saturating_sub(1);
        self.bytes = self.bytes.saturating_sub(size);
    }
    pub fn used(&self) -> (usize, u64) {
        (self.txs, self.bytes)
    }
}

/// Inbound gossip against the two ADR-007 bounds: one transaction at a time, rejected (not scored) when its
/// peer is over Q5 or the admission stage is over Q6. Pair every accepted [`Intake::admit`] with one
/// [`Intake::done`] when the transaction has left admission.
#[derive(Debug, Clone)]
pub struct Intake {
    global: Budget,
    per_peer: BTreeMap<PeerId, Budget>,
}

impl Default for Intake {
    fn default() -> Self {
        Self {
            global: Budget::new(Q6_TXS, Q6_BYTES),
            per_peer: BTreeMap::new(),
        }
    }
}

impl Intake {
    pub fn admit(&mut self, peer: PeerId, size: u64) -> bool {
        let p = self
            .per_peer
            .entry(peer)
            .or_insert_with(|| Budget::new(Q5_PEER_TXS, Q5_PEER_BYTES));
        if !(p.fits(size) && self.global.fits(size)) {
            return false;
        }
        p.take(size);
        self.global.take(size);
        true
    }

    pub fn done(&mut self, peer: PeerId, size: u64) {
        self.global.give(size);
        if let Some(p) = self.per_peer.get_mut(&peer) {
            p.give(size);
            if p.txs == 0 {
                self.per_peer.remove(&peer);
            }
        }
    }

    pub fn global(&self) -> (usize, u64) {
        self.global.used()
    }
}

/// An announced transaction: hash, type and size as the peer says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Announced {
    pub hash: B256,
    pub kind: u8,
    pub size: u32,
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    kind: u8,
    size: u32,
    seq: u64,
}

#[derive(Debug, Default)]
struct Request {
    hashes: Vec<B256>,
    at: u64,
    /// Hashes that arrived by another route while this request was out.
    stolen: BTreeSet<B256>,
    /// Timed out: kept so that a late reply is recognised, never expires.
    dangling: bool,
}

/// What a delivered transaction turned out to be, for the check against its announcements.
#[derive(Debug, Clone, Copy)]
pub struct Delivered {
    pub hash: B256,
    pub kind: u8,
    pub size: u32,
}

/// `TxFetcher` (`eth/fetcher/tx_fetcher.go`): announced hashes wait [`TX_ARRIVE_TIMEOUT_MS`] for the
/// transaction to arrive by broadcast, then are requested, at most [`MAX_TX_RETRIEVALS`] hashes and
/// [`MAX_TX_RETRIEVAL_SIZE`] announced bytes at a time, one request per peer, in the order announced
/// (nonce order tends to follow). A request that is not answered in [`TX_FETCH_TIMEOUT_MS`] moves its
/// hashes to the other peers that announced them. The caller supplies the clock, calls [`Fetcher::tick`]
/// at [`Fetcher::next_deadline`] and sends the [`Effect::Fetch`] it gets back. geth picks the peer of a
/// request by chance (map order); this picks by peer id.
///
/// Not ported: the 32,768-entry set of recently underpriced hashes and the cache of on-chain hashes
/// (`tx_fetcher.go:62-67`, `:83`, `:344-363`); the caller may pass them in `known`.
#[derive(Debug, Default)]
pub struct Fetcher {
    waitlist: BTreeMap<B256, BTreeSet<PeerId>>,
    waittime: BTreeMap<B256, u64>,
    waitslots: BTreeMap<PeerId, BTreeMap<B256, Slot>>,
    announced: BTreeMap<B256, BTreeSet<PeerId>>,
    announces: BTreeMap<PeerId, BTreeMap<B256, Slot>>,
    fetching: BTreeMap<B256, PeerId>,
    alternates: BTreeMap<B256, BTreeSet<PeerId>>,
    requests: BTreeMap<PeerId, Request>,
    seq: u64,
}

impl Fetcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hashes the fetcher holds in any stage (waiting, queued or being fetched).
    pub fn tracked(&self) -> usize {
        self.waitlist.len() + self.announced.len() + self.fetching.len()
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// `Notify` and the announce case of the loop (`tx_fetcher.go:273-343`, `:575-723`). `known` says
    /// whether the pool (or the chain) has the hash.
    pub fn notify(
        &mut self,
        now: u64,
        peer: PeerId,
        anns: &[Announced],
        known: impl Fn(&B256) -> bool,
    ) -> Vec<Effect> {
        let mut unknown: Vec<Announced> =
            anns.iter().filter(|a| !known(&a.hash)).copied().collect();
        if unknown.is_empty() {
            return Vec::new();
        }
        let used = self.waitslots.get(&peer).map_or(0, BTreeMap::len)
            + self.announces.get(&peer).map_or(0, BTreeMap::len);
        if used >= MAX_TX_ANNOUNCES {
            return Vec::new();
        }
        unknown.truncate(MAX_TX_ANNOUNCES - used);
        let old_peer = self.announces.contains_key(&peer);
        for a in unknown {
            let slot = Slot {
                kind: a.kind,
                size: a.size,
                seq: self.next_seq(),
            };
            if let Some(alts) = self.alternates.get_mut(&a.hash) {
                alts.insert(peer);
                self.announces.entry(peer).or_default().insert(a.hash, slot);
            } else if let Some(set) = self.announced.get_mut(&a.hash) {
                set.insert(peer);
                self.announces.entry(peer).or_default().insert(a.hash, slot);
            } else if let Some(set) = self.waitlist.get_mut(&a.hash) {
                if set.insert(peer) {
                    self.waitslots.entry(peer).or_default().insert(a.hash, slot);
                }
            } else {
                self.waitlist.insert(a.hash, BTreeSet::from([peer]));
                self.waittime.insert(a.hash, now);
                self.waitslots.entry(peer).or_default().insert(a.hash, slot);
            }
        }
        if !old_peer && self.announces.get(&peer).is_some_and(|m| !m.is_empty()) {
            return self.schedule(now, Some(BTreeSet::from([peer])));
        }
        Vec::new()
    }

    /// The time at which [`Fetcher::tick`] next has work: the earliest wait or request expiry.
    pub fn next_deadline(&self) -> Option<u64> {
        let w = self
            .waittime
            .values()
            .map(|t| (t + TX_ARRIVE_TIMEOUT_MS).saturating_sub(TX_GATHER_SLACK_MS))
            .min();
        let r = self
            .requests
            .values()
            .filter(|r| !r.dangling)
            .map(|r| (r.at + TX_FETCH_TIMEOUT_MS).saturating_sub(TX_GATHER_SLACK_MS))
            .min();
        w.into_iter().chain(r).min()
    }

    /// The wait and timeout cases of the loop (`tx_fetcher.go:725-803`).
    pub fn tick(&mut self, now: u64) -> Vec<Effect> {
        let mut actives = BTreeSet::new();
        let expired: Vec<B256> = self
            .waittime
            .iter()
            .filter(|(_, t)| now.saturating_sub(**t) + TX_GATHER_SLACK_MS > TX_ARRIVE_TIMEOUT_MS)
            .map(|(h, _)| *h)
            .collect();
        for h in expired {
            let Some(peers) = self.waitlist.remove(&h) else {
                continue;
            };
            self.waittime.remove(&h);
            for p in &peers {
                if let Some(slot) = self.waitslots.get_mut(p).and_then(|m| m.remove(&h)) {
                    self.announces.entry(*p).or_default().insert(h, slot);
                }
                if self.waitslots.get(p).is_some_and(BTreeMap::is_empty) {
                    self.waitslots.remove(p);
                }
                actives.insert(*p);
            }
            self.announced.insert(h, peers);
        }
        let mut out = if actives.is_empty() {
            Vec::new()
        } else {
            self.schedule(now, Some(actives))
        };
        let late: Vec<PeerId> = self
            .requests
            .iter()
            .filter(|(_, r)| {
                !r.dangling && now.saturating_sub(r.at) + TX_GATHER_SLACK_MS > TX_FETCH_TIMEOUT_MS
            })
            .map(|(p, _)| *p)
            .collect();
        if !late.is_empty() {
            for peer in late {
                let hashes = self
                    .requests
                    .get(&peer)
                    .map_or_else(Vec::new, |r| r.hashes.clone());
                let stolen = self
                    .requests
                    .get(&peer)
                    .map_or_else(BTreeSet::new, |r| r.stolen.clone());
                for h in hashes.iter().filter(|h| !stolen.contains(*h)) {
                    if let Some(alts) = self.alternates.remove(h) {
                        self.announced.insert(*h, alts);
                    }
                    if let Some(set) = self.announced.get_mut(h) {
                        set.remove(&peer);
                        if set.is_empty() {
                            self.announced.remove(h);
                        }
                    }
                    if let Some(m) = self.announces.get_mut(&peer) {
                        m.remove(h);
                    }
                    self.fetching.remove(h);
                }
                if self.announces.get(&peer).is_some_and(BTreeMap::is_empty) {
                    self.announces.remove(&peer);
                }
                if let Some(r) = self.requests.get_mut(&peer) {
                    r.hashes.clear();
                    r.dangling = true;
                }
            }
            out.extend(self.schedule(now, None));
        }
        out
    }

    /// `scheduleFetches`, `tx_fetcher.go:1108-1180`.
    fn schedule(&mut self, now: u64, whitelist: Option<BTreeSet<PeerId>>) -> Vec<Effect> {
        let actives: BTreeSet<PeerId> =
            whitelist.unwrap_or_else(|| self.announces.keys().copied().collect());
        let mut out = Vec::new();
        for peer in actives {
            if self.requests.contains_key(&peer) {
                continue;
            }
            let Some(mine) = self.announces.get(&peer) else {
                continue;
            };
            let mut list: Vec<(u64, B256, u32)> =
                mine.iter().map(|(h, s)| (s.seq, *h, s.size)).collect();
            list.sort();
            let mut hashes: Vec<B256> = Vec::new();
            let mut bytes = 0u64;
            for (_, h, size) in list {
                if self.fetching.contains_key(&h) {
                    continue;
                }
                self.fetching.insert(h, peer);
                let alts: BTreeSet<PeerId> =
                    self.announced.remove(&h).into_iter().flatten().collect();
                self.alternates.insert(h, alts);
                hashes.push(h);
                if hashes.len() >= MAX_TX_RETRIEVALS {
                    break;
                }
                bytes += u64::from(size);
                if bytes >= MAX_TX_RETRIEVAL_SIZE {
                    break;
                }
            }
            if !hashes.is_empty() {
                self.requests.insert(
                    peer,
                    Request {
                        hashes: hashes.clone(),
                        at: now,
                        ..Request::default()
                    },
                );
                out.push(Effect::Fetch { peer, hashes });
            }
        }
        out
    }

    /// The `cleanup` case of the loop (`tx_fetcher.go:805-910`): transactions arrived, by broadcast
    /// (`direct` false) or as the reply to the request to `origin` (`direct` true). Returns the next
    /// requests and the peers whose announcement did not match what arrived (wrong type, or a size off by
    /// more than 8 bytes); the caller drops them.
    pub fn delivered(
        &mut self,
        now: u64,
        origin: PeerId,
        txs: &[Delivered],
        direct: bool,
    ) -> (Vec<Effect>, Vec<PeerId>) {
        let mut violators = BTreeSet::new();
        let check = |s: &Slot, d: &Delivered| {
            s.kind != d.kind || (i64::from(s.size) - i64::from(d.size)).abs() > 8
        };
        for d in txs {
            if self.waitlist.remove(&d.hash).is_some() {
                self.waittime.remove(&d.hash);
                for (p, m) in &mut self.waitslots {
                    if let Some(s) = m.remove(&d.hash) {
                        if check(&s, d) {
                            violators.insert(*p);
                        }
                    }
                }
                self.waitslots.retain(|_, m| !m.is_empty());
                continue;
            }
            for (p, m) in &mut self.announces {
                if let Some(s) = m.remove(&d.hash) {
                    if check(&s, d) {
                        violators.insert(*p);
                    }
                }
            }
            self.announces.retain(|_, m| !m.is_empty());
            self.announced.remove(&d.hash);
            self.alternates.remove(&d.hash);
            if let Some(o) = self.fetching.remove(&d.hash) {
                if o != origin || !direct {
                    if let Some(r) = self.requests.get_mut(&o) {
                        r.stolen.insert(d.hash);
                    }
                }
            }
        }
        let mut out = Vec::new();
        if direct {
            if let Some(req) = self.requests.remove(&origin) {
                let got: BTreeSet<B256> = txs.iter().map(|d| d.hash).collect();
                // Nothing delivered: assume everything is missing and do not retry (`cutoff`).
                let cutoff = req
                    .hashes
                    .iter()
                    .rposition(|h| got.contains(h))
                    .unwrap_or(req.hashes.len());
                for (i, h) in req.hashes.iter().enumerate() {
                    if req.stolen.contains(h) {
                        continue;
                    }
                    if !got.contains(h) {
                        if i < cutoff {
                            if let Some(a) = self.alternates.get_mut(h) {
                                a.remove(&origin);
                            }
                            if let Some(m) = self.announces.get_mut(&origin) {
                                m.remove(h);
                            }
                            if self.announces.get(&origin).is_some_and(BTreeMap::is_empty) {
                                self.announces.remove(&origin);
                            }
                        }
                        if let Some(a) = self.alternates.get(h).filter(|a| !a.is_empty()) {
                            self.announced.insert(*h, a.clone());
                        }
                    }
                    self.alternates.remove(h);
                    self.fetching.remove(h);
                }
                out = self.schedule(now, None);
            }
        }
        (out, violators.into_iter().collect())
    }

    /// `Drop`, `tx_fetcher.go:918-990`: forget a peer; what it was asked for goes to the others.
    pub fn drop_peer(&mut self, now: u64, peer: PeerId) -> Vec<Effect> {
        if let Some(slots) = self.waitslots.remove(&peer) {
            for h in slots.keys() {
                if let Some(set) = self.waitlist.get_mut(h) {
                    set.remove(&peer);
                    if set.is_empty() {
                        self.waitlist.remove(h);
                        self.waittime.remove(h);
                    }
                }
            }
        }
        let request = self.requests.remove(&peer);
        if let Some(r) = &request {
            for h in r.hashes.iter().filter(|h| !r.stolen.contains(*h)) {
                if let Some(a) = self.alternates.get_mut(h) {
                    a.remove(&peer);
                }
                match self.alternates.remove(h) {
                    Some(a) if !a.is_empty() => {
                        self.announced.insert(*h, a);
                    }
                    _ => {}
                }
                self.fetching.remove(h);
            }
        }
        if let Some(m) = self.announces.remove(&peer) {
            for h in m.keys() {
                if let Some(a) = self.announced.get_mut(h) {
                    a.remove(&peer);
                    if a.is_empty() {
                        self.announced.remove(h);
                    }
                }
                if let Some(a) = self.alternates.get_mut(h) {
                    a.remove(&peer);
                    if a.is_empty() {
                        self.alternates.remove(h);
                    }
                }
            }
        }
        if request.is_some() {
            return self.schedule(now, None);
        }
        Vec::new()
    }
}
