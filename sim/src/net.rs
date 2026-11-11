//! Deterministic simulator of the V2 BFT protocol over 4 to 14 validators, driving the real step machine
//! (`xone_consensus::v2::bft::Bft`). One seed gives one run; `sim/README.md` states the fault model, the
//! quorum, the clock and the delivery bound that the checks below rely on.
//!
//! Signatures are a hash stand-in (`Stand`): `signer(20) | keccak(secret(signer), digest)(32) | nonce(8)`.
//! `recover` ignores the nonce, so one signer has any number of distinct valid signatures of one message,
//! as with real ECDSA. Everything above the `Scheme` seam is the real code: pools, certificates, the
//! voting rule, the safety unit, the timer, SyncInfo.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use alloy_primitives::{keccak256, Address, Bytes, B256};
use xone_chainspec::{ExpTimeoutConfig, V2Config, V2};
use xone_consensus::v2::{
    bft::{
        Bft, Chain, Config, Effect, EpochFacts, Event, HeaderFacts, Offence, Outbound,
        POOL_HYGIENE_ROUND, VOTE_LOOKAHEAD,
    },
    cert::{min_signers, qc_digest, timeout_digest, unique_signers_with, vote_digest, Scheme},
    commit::Link,
    masternodes,
    safety::{Intent, Kind, SafetyRecord},
    timer::Timer,
    Error,
};
use xone_db::{
    safety::{Fault, MemStore, SafetyStore},
    Durability,
};
use xone_primitives::{BlockInfo, QuorumCert, SyncInfo, Timeout, TimeoutCert, Vote};

use crate::Rng;

const EPOCH: u64 = 900;
/// Ticks between two anti-entropy block pulls of a node. A pull brings every block the peer holds and the node lacks.
const SYNC_EVERY: u64 = 5;

// ---------------------------------------------------------------- the signature stand-in

fn addr(i: usize) -> Address {
    Address::from_slice(&keccak256([b"validator".as_slice(), &i.to_be_bytes()].concat())[12..])
}

fn mac(a: &Address, d: &B256) -> B256 {
    let secret = keccak256([b"sim-key".as_slice(), a.as_slice()].concat());
    keccak256([secret.as_slice(), d.as_slice()].concat())
}

fn sign(a: &Address, d: &B256, nonce: u64) -> Bytes {
    [a.as_slice(), mac(a, d).as_slice(), &nonce.to_be_bytes()]
        .concat()
        .into()
}

#[derive(Clone, Copy)]
struct Stand;

impl Scheme for Stand {
    fn recover(&self, digest: &B256, sig: &[u8]) -> Result<Address, Error> {
        let (Some(a), Some(m)) = (sig.get(..20), sig.get(20..52)) else {
            return Err(Error::SignatureUnrecoverable);
        };
        let a = Address::from_slice(a);
        (mac(&a, digest).as_slice() == m)
            .then_some(a)
            .ok_or(Error::SignatureUnrecoverable)
    }
}

// ---------------------------------------------------------------- the scenario of a seed

#[derive(Clone, Debug)]
pub struct Scenario {
    pub n: usize,
    pub byz: Vec<usize>,
    /// Quorum: the smallest signer count the config threshold accepts (`cert::min_signers`).
    pub need: usize,
    pub gst: u64,
    pub delta_pre: u64,
    pub delta_post: u64,
    pub loss_pct: u64,
    pub dup_pct: u64,
    pub partitions: Vec<(u64, u64, BTreeSet<usize>)>,
    pub kills: Vec<(u64, usize)>,
    pub step_kill_pct: u64,
    pub rows: Vec<(u64, V2Config)>,
    /// The recovery bound in ticks after GST, from the timeout schedule of `rows`.
    pub bound: u64,
    pub t_max: u64,
}

impl Scenario {
    pub fn new(seed: u64) -> Self {
        let mut r = Rng(seed ^ 0xD1CE_F00D);
        let n = 4 + r.below(11) as usize;
        let f = (n - 1) / 3;
        let nb = r.below(f as u64 + 1) as usize;
        let mut byz = BTreeSet::new();
        while byz.len() < nb {
            byz.insert(r.below(n as u64) as usize);
        }
        let row = |period, base, exp, sync, mine| V2Config {
            timeout_period: period,
            timeout_sync_threshold: sync,
            mine_period: mine,
            cert_threshold: 0.667,
            max_masternodes: 108,
            exp_timeout: ExpTimeoutConfig {
                base,
                max_exponent: exp,
            },
            ..V2Config::default()
        };
        let first = row(
            [10, 12, 20][r.below(3) as usize],
            [1.0, 1.0, 2.0, 1.5][r.below(4) as usize],
            [0, 3, 4][r.below(3) as usize],
            2 + r.below(2),
            1 + r.below(2),
        );
        let mut rows = vec![(0, first)];
        if r.pct(50) {
            let second = row(
                [10, 15][r.below(2) as usize],
                [1.0, 2.0][r.below(2) as usize],
                [0, 2][r.below(2) as usize],
                2 + r.below(3),
                1 + r.below(2),
            );
            rows.push((20 + r.below(40), second));
        }
        let gst = 40 + r.below(140);
        let delta_pre = 1 + r.below(8);
        let delta_post = 1 + r.below(2);
        let mut partitions = vec![];
        for _ in 0..r.below(3) {
            let s = r.below(gst);
            let e = (s + 5 + r.below(60)).min(gst);
            let g: BTreeSet<usize> = (0..n).filter(|_| r.pct(50)).collect();
            partitions.push((s, e, g));
        }
        let mut kills = vec![];
        for _ in 0..r.below(4) {
            kills.push((
                r.below(gst.saturating_sub(40).max(1)),
                r.below(n as u64) as usize,
            ));
        }
        let need = min_signers(n, 0.667).unwrap_or(n);
        // the longest timer delay of any row: the schedule itself, from the real timer
        let mut t_max = 0;
        for (_, row) in &rows {
            let mut t = Timer::new(row);
            t.set_params(row);
            for k in 0..=u64::from(row.exp_timeout.max_exponent) {
                t_max = t_max.max(t.reset(k, 0).1.as_secs_f64().ceil() as u64);
            }
        }
        let sync = rows
            .iter()
            .map(|(_, r)| r.timeout_sync_threshold)
            .max()
            .unwrap_or(3);
        let mine = rows.iter().map(|(_, r)| r.mine_period).max().unwrap_or(2);
        let d = delta_post;
        // after GST: the nodes first meet in one round (the nodes ahead time out `sync` times and send
        // SyncInfo; twice, once to bring the leaders' highest QC), then at most every Byzantine leader and every
        // honest leader whose QC was stale burn a round of two timer periods, then three good rounds commit.
        let sync_wait = 2 * sync * t_max + 4 * d + 2 * (SYNC_EVERY + d);
        let failed_round = 2 * t_max + 3 * d + mine;
        let bound = sync_wait + (n as u64 + 3) * failed_round;
        Self {
            n,
            byz: byz.into_iter().collect(),
            need,
            gst,
            delta_pre,
            delta_post,
            loss_pct: r.below(45),
            dup_pct: r.below(20),
            partitions,
            kills,
            step_kill_pct: r.below(4),
            rows,
            bound,
            t_max,
        }
    }
}

// ---------------------------------------------------------------- what everyone shares: blocks and the oracle's facts

struct World {
    blocks: HashMap<B256, Arc<HeaderFacts>>,
    proposer: HashMap<B256, usize>,
    by_round: HashMap<u64, Vec<B256>>,
    /// The vote digest of each block, to find the block of an intent.
    by_digest: HashMap<B256, B256>,
    genesis: B256,
    /// Genuine votes: who signed which block in which round.
    voters: HashMap<(u64, B256), BTreeMap<usize, Bytes>>,
    /// Quorum certificates that exist, by round.
    qcs: BTreeMap<u64, QuorumCert>,
    timeouters: HashMap<u64, BTreeSet<usize>>,
    /// The highest round of any QC or TC that exists. No honest node may be above this plus 1.
    cert_round: u64,
    need: usize,
}

impl World {
    fn add(&mut self, h: HeaderFacts, proposer: usize) -> Arc<HeaderFacts> {
        let h = Arc::new(h);
        self.blocks.insert(h.hash, h.clone());
        self.proposer.insert(h.hash, proposer);
        self.by_round.entry(h.round).or_default().push(h.hash);
        let info = BlockInfo {
            hash: h.hash,
            round: h.round,
            number: h.number,
        };
        self.by_digest.insert(vote_digest(&info, 0), h.hash);
        h
    }

    fn note_vote(&mut self, who: usize, v: &Vote) -> Result<(), String> {
        let i = &v.proposed_block_info;
        let e = self.voters.entry((i.round, i.hash)).or_default();
        e.entry(who).or_insert_with(|| v.signature.clone());
        if e.len() >= self.need
            && !self
                .qcs
                .get(&i.round)
                .is_some_and(|q| q.proposed_block_info == *i)
        {
            if let Some(q) = self.qcs.get(&i.round) {
                return Err(format!(
                    "two blocks have a quorum in round {}: {} and {}",
                    i.round, q.proposed_block_info.hash, i.hash
                ));
            }
            let qc = QuorumCert {
                proposed_block_info: i.clone(),
                signatures: e.values().cloned().collect(),
                gap_number: 0,
            };
            self.qcs.insert(i.round, qc);
            self.cert_round = self.cert_round.max(i.round);
        }
        Ok(())
    }

    fn note_timeout(&mut self, who: usize, round: u64) {
        let e = self.timeouters.entry(round).or_default();
        e.insert(who);
        if e.len() >= self.need {
            self.cert_round = self.cert_round.max(round);
        }
    }

    fn is_ancestor(&self, anc: &B256, of: &B256) -> bool {
        let (Some(a), mut cur) = (self.blocks.get(anc), *of) else {
            return false;
        };
        while let Some(h) = self.blocks.get(&cur) {
            if h.number < a.number {
                return false;
            }
            if cur == *anc {
                return true;
            }
            cur = h.parent;
        }
        false
    }
}

/// One node's view of the chain: the blocks it has executed.
struct View<'a> {
    w: &'a World,
    have: &'a HashSet<B256>,
    head: B256,
    epoch: &'a Arc<EpochFacts>,
    candidates: &'a Arc<[Address]>,
}

impl Chain for View<'_> {
    fn header(&self, hash: &B256) -> Option<Arc<HeaderFacts>> {
        if self.have.contains(hash) {
            self.w.blocks.get(hash).cloned()
        } else {
            None
        }
    }
    fn head(&self) -> Option<Arc<HeaderFacts>> {
        self.w.blocks.get(&self.head).cloned()
    }
    fn epoch(&self, hash: &B256) -> Result<Arc<EpochFacts>, Error> {
        if self.have.contains(hash) {
            Ok(self.epoch.clone())
        } else {
            Err(Error::MissingHeader { hash: *hash })
        }
    }
    fn epoch_of_round(&self, _: u64) -> Result<Arc<EpochFacts>, Error> {
        Ok(self.epoch.clone())
    }
    fn candidates(&self, _: u64) -> Option<Arc<[Address]>> {
        Some(self.candidates.clone())
    }
    fn timeout_gap(&self, _: u64) -> Result<u64, Error> {
        Ok(0)
    }
}

// ---------------------------------------------------------------- nodes, messages, events

enum Msg {
    Block(Arc<HeaderFacts>, usize),
    Vote(Vote),
    Timeout(Timeout),
    Sync(SyncInfo),
}

enum Ev {
    Deliver {
        to: usize,
        from: usize,
        msg: Arc<Msg>,
    },
    Timer {
        node: usize,
        token: u64,
    },
    Propose {
        node: usize,
        round: u64,
    },
    Kill {
        node: usize,
    },
    Restart {
        node: usize,
    },
    SyncTick {
        node: usize,
    },
    ByzTick {
        node: usize,
    },
}

struct Node {
    addr: Address,
    byz: bool,
    bft: Option<Bft<Stand>>,
    store: MemStore,
    /// The last record the store acknowledged or a restart loaded.
    acked: SafetyRecord,
    have: HashSet<B256>,
    head: B256,
    /// Blocks waiting for their parent, by parent hash.
    pending: HashMap<B256, Vec<(Arc<HeaderFacts>, usize)>>,
    /// Rounds this leader sealed (the producer's own guard, XB-23).
    proposed: HashSet<u64>,
    committed: u64,
    /// The block voted at each height, for the count of sibling votes.
    voted_at: HashMap<u64, B256>,
    last_qc: (u64, B256),
    max_timeout: u64,
    /// The highest QC and TC the driver stored (`Effect::Store`); they survive a restart.
    stored: Option<SyncInfo>,
}

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub votes: u64,
    pub timeouts: u64,
    pub sync_infos: u64,
    pub blocks: u64,
    pub sibling_votes: u64,
    pub commits: u64,
    pub rounds: u64,
    pub kills: [u64; 4],
    pub restarts: u64,
    pub dropped: u64,
    pub refused: u64,
    pub scored_invalid: u64,
    pub scored_oversized: u64,
    pub byz_variants: u64,
    pub byz_equivocations: u64,
    pub byz_lock_attacks: u64,
    pub byz_forged_sync: u64,
}

impl Stats {
    pub fn add(&mut self, o: &Stats) {
        self.votes += o.votes;
        self.timeouts += o.timeouts;
        self.sync_infos += o.sync_infos;
        self.blocks += o.blocks;
        self.sibling_votes += o.sibling_votes;
        self.commits += o.commits;
        self.rounds += o.rounds;
        for (a, b) in self.kills.iter_mut().zip(o.kills) {
            *a += b;
        }
        self.restarts += o.restarts;
        self.dropped += o.dropped;
        self.refused += o.refused;
        self.scored_invalid += o.scored_invalid;
        self.scored_oversized += o.scored_oversized;
        self.byz_variants += o.byz_variants;
        self.byz_equivocations += o.byz_equivocations;
        self.byz_lock_attacks += o.byz_lock_attacks;
        self.byz_forged_sync += o.byz_forged_sync;
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    pub n: usize,
    pub byz: usize,
    pub gst: u64,
    pub bound: u64,
    /// Ticks after GST at which every honest node had committed a block above the one committed at GST.
    pub recovery: Option<u64>,
    pub stats: Stats,
}

struct Sim {
    sc: Scenario,
    rng: Rng,
    t: u64,
    seq: u64,
    q: BTreeMap<(u64, u64), Ev>,
    w: World,
    nodes: Vec<Node>,
    epoch: Arc<EpochFacts>,
    candidates: Arc<[Address]>,
    cfg: Config,
    stats: Stats,
    /// Signatures that left an honest node: (node, kind, round) -> digest.
    emitted: HashMap<(usize, u8, u64), B256>,
    highest: B256,
    base: Option<u64>,
    recovery: Option<u64>,
    /// Print the run (the `trace` example).
    trace: bool,
}

fn kind_idx(k: Kind) -> u8 {
    match k {
        Kind::Vote => 0,
        Kind::Timeout => 1,
        Kind::Propose => 2,
    }
}

impl Sim {
    fn new(seed: u64) -> Self {
        let sc = Scenario::new(seed);
        let mut w = World {
            blocks: HashMap::new(),
            proposer: HashMap::new(),
            by_round: HashMap::new(),
            by_digest: HashMap::new(),
            genesis: B256::ZERO,
            voters: HashMap::new(),
            qcs: BTreeMap::new(),
            timeouters: HashMap::new(),
            cert_round: 0,
            need: sc.need,
        };
        let genesis = keccak256(b"genesis");
        w.add(
            HeaderFacts {
                number: 0,
                hash: genesis,
                parent: B256::ZERO,
                round: 0,
                qc: None,
            },
            usize::MAX,
        );
        w.genesis = genesis;
        let masternodes: Vec<Address> = (0..sc.n).map(addr).collect();
        let epoch = Arc::new(EpochFacts {
            masternodes: masternodes.clone(),
            gap: 0,
            switch: genesis,
        });
        let candidates: Arc<[Address]> = masternodes.into();
        let cfg = Config {
            v2: V2 {
                switch_block: 0,
                switch_epoch: 1,
                configs: sc.rows.iter().cloned().collect(),
            },
            gap: 450,
            me: None,
        };
        let nodes = (0..sc.n)
            .map(|i| Node {
                addr: addr(i),
                byz: sc.byz.contains(&i),
                bft: None,
                store: MemStore::default(),
                acked: SafetyRecord::new(addr(i)),
                have: HashSet::from([genesis]),
                head: genesis,
                pending: HashMap::new(),
                proposed: HashSet::new(),
                committed: 0,
                voted_at: HashMap::new(),
                last_qc: (0, genesis),
                max_timeout: 0,
                stored: None,
            })
            .collect();
        Self {
            rng: Rng(seed),
            t: 0,
            seq: 0,
            q: BTreeMap::new(),
            w,
            nodes,
            epoch,
            candidates,
            cfg,
            stats: Stats::default(),
            emitted: HashMap::new(),
            highest: genesis,
            base: None,
            recovery: None,
            trace: false,
            sc,
        }
    }

    fn push(&mut self, at: u64, ev: Ev) {
        self.seq += 1;
        self.q.insert((at, self.seq), ev);
    }

    fn honest(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.sc.n).filter(|i| !self.nodes[*i].byz)
    }

    // ------------------------------------------------------------ the network

    fn partitioned(&self, a: usize, b: usize) -> bool {
        self.sc
            .partitions
            .iter()
            .any(|(s, e, g)| *s <= self.t && self.t < *e && g.contains(&a) != g.contains(&b))
    }

    /// Before GST a message may be lost, duplicated, delayed up to `delta_pre` and cut by a partition.
    /// From GST every message is delivered after 1 to `delta_post` ticks. A message to oneself is local.
    fn send(&mut self, from: usize, to: usize, msg: Arc<Msg>) {
        let t = self.t;
        if from == to {
            return self.push(t, Ev::Deliver { to, from, msg });
        }
        if t >= self.sc.gst {
            let d = 1 + self.rng.below(self.sc.delta_post);
            return self.push(t + d, Ev::Deliver { to, from, msg });
        }
        if self.rng.pct(self.sc.loss_pct) || self.partitioned(from, to) {
            self.stats.dropped += 1;
            return;
        }
        let d = 1 + self.rng.below(self.sc.delta_pre);
        if self.rng.pct(self.sc.dup_pct) {
            let d2 = 1 + self.rng.below(self.sc.delta_pre);
            self.push(
                t + d2,
                Ev::Deliver {
                    to,
                    from,
                    msg: msg.clone(),
                },
            );
        }
        self.push(t + d, Ev::Deliver { to, from, msg });
    }

    fn broadcast(&mut self, from: usize, msg: Msg) {
        let msg = Arc::new(msg);
        for to in 0..self.sc.n {
            self.send(from, to, msg.clone());
        }
    }

    // ------------------------------------------------------------ nodes

    fn start(&mut self, id: usize) -> Result<(), String> {
        let rec = match self.nodes[id]
            .store
            .load()
            .map_err(|e| format!("load: {e}"))?
        {
            Some(l) => SafetyRecord::decode(&l.record).map_err(|e| format!("decode: {e}"))?,
            None => SafetyRecord::new(self.nodes[id].addr),
        };
        let mut cfg = self.cfg.clone();
        cfg.me = Some(self.nodes[id].addr);
        self.nodes[id].acked = rec.clone();
        self.nodes[id].bft = Some(Bft::new(cfg, Stand, rec).map_err(|e| e.to_string())?);
        self.step(id, Event::Synced)?;
        // the stored SyncInfo comes back as a SyncInfo of the node to itself: verified like any other
        if let Some(si) = self.nodes[id].stored.clone() {
            self.step(id, Event::SyncInfo(si, None))?;
        }
        Ok(())
    }

    fn die(&mut self, id: usize) -> Result<(), String> {
        let n = &mut self.nodes[id];
        n.bft = None;
        let d = 1 + self.rng.below(20);
        self.stats.restarts += 1;
        self.push(self.t + d, Ev::Restart { node: id });
        Ok(())
    }

    /// One event into an honest node, then its effects.
    fn step(&mut self, id: usize, ev: Event) -> Result<(), String> {
        let is_sync = self.trace && matches!(ev, Event::SyncInfo(..));
        let fx = {
            let Sim {
                w,
                nodes,
                epoch,
                candidates,
                ..
            } = self;
            let node = &mut nodes[id];
            let Some(bft) = node.bft.as_mut() else {
                return Ok(());
            };
            let view = View {
                w,
                have: &node.have,
                head: node.head,
                epoch,
                candidates,
            };
            bft.step(&view, Durability::Durable, ev)
        };
        if is_sync {
            eprintln!("t={} node {id} handled a SyncInfo: {fx:?}", self.t);
        }
        self.check_state(id)?;
        self.exec(id, fx)
    }

    /// State checks after each step of an honest node.
    fn check_state(&mut self, id: usize) -> Result<(), String> {
        let node = &mut self.nodes[id];
        let Some(bft) = node.bft.as_ref() else {
            return Ok(());
        };
        // no round without a certificate (forged SyncInfo, duplicate-counting pools)
        if bft.round() > self.w.cert_round + 1 {
            return Err(format!(
                "node {id} is in round {} but no QC or TC above round {} exists",
                bft.round(),
                self.w.cert_round
            ));
        }
        // every QC the node keeps has a quorum of distinct masternode signers
        let qc = bft.highest_qc();
        let key = (qc.proposed_block_info.round, qc.proposed_block_info.hash);
        if key != node.last_qc {
            if key.0 > 0 {
                let d = qc_digest(qc);
                let ok = unique_signers_with(&Stand, &d, &qc.signatures, &self.candidates)
                    .map_or(0, |s| s.len())
                    >= self.sc.need;
                if !ok {
                    return Err(format!(
                        "node {id} holds a QC of round {} without a quorum of signers",
                        key.0
                    ));
                }
            }
            node.last_qc = key;
        }
        let (pv, pt) = bft.pooled();
        let cap = (VOTE_LOOKAHEAD + POOL_HYGIENE_ROUND + 2) as usize * self.sc.n;
        if pv > cap || pt > self.sc.n {
            return Err(format!(
                "node {id}: pools hold {pv} votes and {pt} timeouts"
            ));
        }
        Ok(())
    }

    /// Run the effects of one step in order. A kill ends the run of the list at its point.
    fn exec(&mut self, id: usize, fx: Vec<Effect>) -> Result<(), String> {
        let can_kill = self.t + 21 < self.sc.gst;
        let kill = (can_kill && self.rng.pct(self.sc.step_kill_pct)).then(|| self.rng.below(4));
        let mut persisted: Vec<Intent> = vec![];
        let mut sig: Option<Bytes> = None;
        for e in fx {
            match e {
                Effect::PersistIntent { intent, record } => {
                    if kill == Some(0) {
                        self.stats.kills[0] += 1;
                        return self.die(id);
                    }
                    if kill == Some(3) {
                        self.stats.kills[3] += 1;
                        let f = [
                            Fault::BeforeWrite,
                            Fault::AfterWrite,
                            Fault::FailSync,
                            Fault::AfterSync,
                        ][self.rng.below(4) as usize];
                        self.nodes[id].store.arm(f);
                    }
                    let n = &mut self.nodes[id];
                    if n.store
                        .persist_intent(&intent.to_bytes(), &record.encode())
                        .is_err()
                    {
                        let keep = self.rng.pct(50);
                        self.nodes[id].store.power_cycle(keep);
                        return self.die(id);
                    }
                    n.acked = record;
                    persisted.push(intent);
                    if kill == Some(1) {
                        self.stats.kills[1] += 1;
                        return self.die(id);
                    }
                }
                Effect::Sign(i) => {
                    // a signature needs the record on disk first, or a retry that the record holds
                    let slot = self.nodes[id].acked.signed[kind_idx(i.kind) as usize];
                    let retry = slot.round == i.round && slot.digest == i.digest;
                    if !persisted.contains(&i) && !retry {
                        return Err(format!(
                            "node {id} signed {:?} of round {} before it was persisted",
                            i.kind, i.round
                        ));
                    }
                    let key = (id, kind_idx(i.kind), i.round);
                    if let Some(old) = self.emitted.insert(key, i.digest) {
                        if old != i.digest {
                            return Err(format!(
                                "node {id} signed {:?} of round {} twice with different digests",
                                i.kind, i.round
                            ));
                        }
                    }
                    if i.kind == Kind::Vote && i.round <= self.nodes[id].max_timeout {
                        return Err(format!(
                            "node {id} voted in round {} after a timeout of round {}",
                            i.round, self.nodes[id].max_timeout
                        ));
                    }
                    if i.kind == Kind::Timeout {
                        let n = &mut self.nodes[id];
                        n.max_timeout = n.max_timeout.max(i.round);
                    }
                    if i.kind == Kind::Vote {
                        // the voting rule, against the record's lock and the real ancestry
                        let lock = self.nodes[id].acked.lock_qc;
                        if let Some(l) = lock {
                            let block = self
                                .w
                                .by_digest
                                .get(&i.digest)
                                .and_then(|h| self.w.blocks.get(h));
                            let Some(b) = block else {
                                return Err("a vote for a block that does not exist".into());
                            };
                            if i.ctx.qc_round <= l.round && !self.w.is_ancestor(&l.hash, &b.hash) {
                                return Err(format!(
                                    "node {id} voted block {} of round {} that does not extend its lock (round {}) with a QC of round {}",
                                    b.hash, b.round, l.round, i.ctx.qc_round
                                ));
                            }
                        }
                    }
                    sig = Some(sign(&self.nodes[id].addr, &i.digest, 0));
                }
                Effect::Send(out) => {
                    let s = sig.take();
                    self.send_out(id, out, s)?;
                    if kill == Some(2) {
                        self.stats.kills[2] += 1;
                        return self.die(id);
                    }
                }
                Effect::SetTimer { token, after } => {
                    let ticks = after.as_secs_f64().ceil() as u64;
                    self.push(self.t + ticks.max(1), Ev::Timer { node: id, token });
                }
                Effect::AdvanceRound { round, mine_period } => {
                    if self.trace {
                        eprintln!("t={} node {id} enters round {round}", self.t);
                    }
                    self.stats.rounds = self.stats.rounds.max(round);
                    let mns = &self.epoch.masternodes;
                    if masternodes::leader(round, EPOCH, mns).ok() == Some(self.nodes[id].addr) {
                        self.push(self.t + mine_period.max(1), Ev::Propose { node: id, round });
                    }
                }
                Effect::Commit(link) => self.on_commit(id, link)?,
                Effect::Store(si) => self.nodes[id].stored = Some(si),
                Effect::ScorePeer { peer, offence } => {
                    let peer = usize::try_from(peer).map_err(|e| e.to_string())?;
                    if !self.nodes[peer].byz {
                        return Err(format!("honest node {peer} was scored by node {id}"));
                    }
                    match offence {
                        Offence::Invalid => self.stats.scored_invalid += 1,
                        Offence::Oversized => self.stats.scored_oversized += 1,
                    }
                }
                Effect::Forensics(p) => {
                    return Err(format!(
                        "forensics event {} with at most f Byzantine validators",
                        p.id
                    ));
                }
                Effect::Refused { .. } => self.stats.refused += 1,
            }
        }
        Ok(())
    }

    fn send_out(&mut self, id: usize, out: Outbound, sig: Option<Bytes>) -> Result<(), String> {
        if self.trace {
            let what = match &out {
                Outbound::Vote { info, .. } => format!("vote r{} {}", info.round, info.number),
                Outbound::Timeout { round, .. } => format!("timeout r{round}"),
                Outbound::SyncInfo(s) => format!(
                    "syncinfo qc r{} tc r{}",
                    s.highest_quorum_cert.proposed_block_info.round, s.highest_timeout_cert.round
                ),
            };
            eprintln!("t={} node {id} sends {what}", self.t);
        }
        match out {
            Outbound::Vote { info, gap } => {
                let v = Vote {
                    proposed_block_info: info.clone(),
                    signature: sig.ok_or("a vote without a signature")?,
                    gap_number: gap,
                };
                self.w.note_vote(id, &v)?;
                self.stats.votes += 1;
                if let Some(old) = self.nodes[id].voted_at.insert(info.number, info.hash) {
                    self.stats.sibling_votes += u64::from(old != info.hash);
                }
                self.broadcast(id, Msg::Vote(v));
            }
            Outbound::Timeout { round, gap } => {
                let t = Timeout {
                    round,
                    signature: sig.ok_or("a timeout without a signature")?,
                    gap_number: gap,
                };
                self.w.note_timeout(id, round);
                self.stats.timeouts += 1;
                self.broadcast(id, Msg::Timeout(t));
            }
            Outbound::SyncInfo(s) => {
                self.stats.sync_infos += 1;
                self.broadcast(id, Msg::Sync(s));
            }
        }
        Ok(())
    }

    fn on_commit(&mut self, id: usize, link: Link) -> Result<(), String> {
        if self.trace {
            eprintln!(
                "t={} node {id} commits {} (round {})",
                self.t, link.number, link.round
            );
        }
        self.stats.commits += 1;
        self.nodes[id].committed = self.nodes[id].committed.max(link.number);
        let hi = self.w.blocks[&self.highest].clone();
        if link.number <= hi.number {
            if !self.w.is_ancestor(&link.hash, &self.highest) {
                return Err(format!(
                    "conflicting commits: {} at {} and {} at {}",
                    link.hash, link.number, hi.hash, hi.number
                ));
            }
        } else {
            if !self.w.is_ancestor(&self.highest, &link.hash) {
                return Err(format!(
                    "conflicting commits: {} at {} and {} at {}",
                    link.hash, link.number, hi.hash, hi.number
                ));
            }
            self.highest = link.hash;
        }
        if self.t >= self.sc.gst && self.recovery.is_none() {
            let base = self.base.unwrap_or(0);
            if self.honest().all(|i| self.nodes[i].committed > base) {
                self.recovery = Some(self.t - self.sc.gst);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ blocks

    /// The header check a block passes before the engine sees it (XB-11): the leader of its round,
    /// a QC of its parent with a quorum, a round above the QC's, the next height.
    fn valid_block(&self, h: &HeaderFacts, proposer: usize) -> bool {
        let Some(qc) = &h.qc else {
            return false;
        };
        let mns = &self.epoch.masternodes;
        let leader = masternodes::leader(h.round, EPOCH, mns).ok();
        let qc_ok = qc.proposed_block_info.round == 0
            || unique_signers_with(&Stand, &qc_digest(qc), &qc.signatures, mns)
                .map_or(0, |s| s.len())
                >= self.sc.need;
        leader == mns.get(proposer).copied()
            && h.round > qc.proposed_block_info.round
            && qc.proposed_block_info.hash == h.parent
            && self
                .w
                .blocks
                .get(&h.parent)
                .is_some_and(|p| p.number + 1 == h.number)
            && qc_ok
    }

    /// Import a block and the blocks that waited for it. `fresh` blocks (from their proposer) are handed to
    /// the engine as proposals; so is any block of the node's current round or above.
    fn import(
        &mut self,
        id: usize,
        h: Arc<HeaderFacts>,
        proposer: usize,
        fresh: bool,
    ) -> Result<(), String> {
        let mut todo = vec![(h, proposer, fresh)];
        while let Some((h, proposer, fresh)) = todo.pop() {
            if self.nodes[id].have.contains(&h.hash) {
                continue;
            }
            if !self.nodes[id].have.contains(&h.parent) {
                self.nodes[id]
                    .pending
                    .entry(h.parent)
                    .or_default()
                    .push((h, proposer));
                continue;
            }
            if !self.valid_block(&h, proposer) {
                continue;
            }
            let n = &mut self.nodes[id];
            n.have.insert(h.hash);
            let hd = &self.w.blocks[&n.head];
            if (h.number, h.round) > (hd.number, hd.round) {
                n.head = h.hash;
            }
            if let Some(children) = n.pending.remove(&h.hash) {
                todo.extend(children.into_iter().map(|(c, p)| (c, p, false)));
            }
            let cur = self.nodes[id].bft.as_ref().map_or(u64::MAX, Bft::round);
            if !self.nodes[id].byz && (fresh || h.round >= cur) {
                self.step(id, Event::Proposal(h.hash))?;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ the event loop

    fn deliver(&mut self, to: usize, from: usize, msg: Arc<Msg>) -> Result<(), String> {
        if self.nodes[to].bft.is_none() && !self.nodes[to].byz {
            return Ok(()); // down
        }
        match &*msg {
            Msg::Block(h, p) => self.import(to, h.clone(), *p, from == *p)?,
            _ if self.nodes[to].byz => {}
            Msg::Vote(v) => self.step(to, Event::Vote(v.clone(), Some(from as u64)))?,
            Msg::Timeout(t) => self.step(to, Event::Timeout(t.clone(), Some(from as u64)))?,
            Msg::Sync(s) => self.step(to, Event::SyncInfo(s.clone(), Some(from as u64)))?,
        }
        Ok(())
    }

    fn propose(&mut self, id: usize, round: u64) -> Result<(), String> {
        let n = &self.nodes[id];
        let Some(bft) = n.bft.as_ref() else {
            return Ok(());
        };
        if bft.round() != round || n.proposed.contains(&round) {
            return Ok(());
        }
        let parent = bft.highest_qc().proposed_block_info.hash;
        if !n.have.contains(&parent) {
            return Ok(());
        }
        let qc = bft.highest_qc().clone();
        let p = self.w.blocks[&parent].clone();
        let hash = keccak256(
            [
                p.hash.as_slice(),
                &round.to_be_bytes(),
                &id.to_be_bytes(),
                &[0],
            ]
            .concat(),
        );
        let h = self.w.add(
            HeaderFacts {
                number: p.number + 1,
                hash,
                parent,
                round,
                qc: Some(qc),
            },
            id,
        );
        self.stats.blocks += 1;
        if self.trace {
            eprintln!(
                "t={} node {id} seals block {} round {round}",
                self.t, h.number
            );
        }
        self.nodes[id].proposed.insert(round);
        self.nodes[id].have.insert(hash);
        let hd = &self.w.blocks[&self.nodes[id].head];
        if (h.number, h.round) > (hd.number, hd.round) {
            self.nodes[id].head = hash;
        }
        self.broadcast(id, Msg::Block(h, id));
        self.step(id, Event::Proposal(hash))
    }

    fn sync_tick(&mut self, id: usize) {
        self.push(self.t + SYNC_EVERY, Ev::SyncTick { node: id });
        if self.nodes[id].bft.is_none() {
            return;
        }
        // before GST a pull goes to one random peer; from GST to every honest peer, so a block held by an
        // honest node reaches every honest node within `SYNC_EVERY` plus the delivery bound
        let peers: Vec<usize> = if self.t >= self.sc.gst {
            self.honest().filter(|p| *p != id).collect()
        } else {
            vec![self.rng.below(self.sc.n as u64) as usize]
        };
        for peer in peers {
            if peer == id || self.nodes[peer].byz {
                continue;
            }
            let mut missing: Vec<_> = self.nodes[peer]
                .have
                .difference(&self.nodes[id].have)
                .filter_map(|h| self.w.blocks.get(h).cloned())
                .collect();
            missing.sort_by_key(|h| h.number);
            for h in missing {
                let p = self.w.proposer[&h.hash];
                self.send(peer, id, Arc::new(Msg::Block(h, p)));
            }
        }
    }

    fn byz_tick(&mut self, id: usize) {
        let again = self.t + 1 + self.rng.below(3);
        self.push(again, Ev::ByzTick { node: id });
        let r = self.w.cert_round + 1;
        let me = self.nodes[id].addr;
        let honest: Vec<usize> = self.honest().collect();
        match self.rng.below(10) {
            // votes for the blocks of the round: one, or all of them (equivocation), with signature variants
            0..=3 => {
                let blocks = self.w.by_round.get(&r).cloned().unwrap_or_default();
                if blocks.is_empty() {
                    return;
                }
                let all = self.rng.pct(50);
                for h in blocks
                    .iter()
                    .filter(|_| all)
                    .chain(blocks.first().filter(|_| !all))
                {
                    let hd = self.w.blocks[h].clone();
                    let info = BlockInfo {
                        hash: hd.hash,
                        round: hd.round,
                        number: hd.number,
                    };
                    self.stats.byz_equivocations += u64::from(all && blocks.len() > 1);
                    for nonce in 0..=self.rng.below(5) {
                        let v = Vote {
                            proposed_block_info: info.clone(),
                            signature: sign(&me, &vote_digest(&info, 0), nonce),
                            gap_number: 0,
                        };
                        self.stats.byz_variants += u64::from(nonce > 0);
                        let _ = self.w.note_vote(id, &v);
                        let msg = Arc::new(Msg::Vote(v));
                        for to in honest
                            .iter()
                            .filter(|_| self.rng.pct(80))
                            .copied()
                            .collect::<Vec<_>>()
                        {
                            self.send(id, to, msg.clone());
                        }
                    }
                }
            }
            4..=5 => {
                for nonce in 0..=self.rng.below(4) {
                    let t = Timeout {
                        round: r,
                        signature: sign(&me, &timeout_digest(r, 0), nonce),
                        gap_number: 0,
                    };
                    self.stats.byz_variants += u64::from(nonce > 0);
                    self.w.note_timeout(id, r);
                    let msg = Arc::new(Msg::Timeout(t));
                    for &to in &honest {
                        self.send(id, to, msg.clone());
                    }
                }
            }
            // as leader: two blocks on the highest QC, or a block on an older QC (the lock attack)
            6 => {
                let mns = &self.epoch.masternodes;
                if masternodes::leader(r, EPOCH, mns).ok() != Some(me) {
                    return;
                }
                let qcs: Vec<_> = self.w.qcs.values().cloned().collect();
                let Some(top) = qcs.last().cloned().or_else(|| {
                    self.nodes[id]
                        .have
                        .get(&self.w.genesis)
                        .map(|g| QuorumCert {
                            proposed_block_info: BlockInfo {
                                hash: *g,
                                round: 0,
                                number: 0,
                            },
                            signatures: vec![],
                            gap_number: 0,
                        })
                }) else {
                    return;
                };
                let attack = qcs.len() > 2 && self.rng.pct(50);
                let qc = if attack {
                    qcs[self.rng.below(qcs.len() as u64 - 1) as usize].clone()
                } else {
                    top
                };
                self.stats.byz_lock_attacks += u64::from(attack);
                let Some(parent) = self.w.blocks.get(&qc.proposed_block_info.hash).cloned() else {
                    return;
                };
                for salt in 0..(1 + self.rng.below(2)) as u8 {
                    let hash = keccak256(
                        [
                            parent.hash.as_slice(),
                            &r.to_be_bytes(),
                            &id.to_be_bytes(),
                            &[salt],
                        ]
                        .concat(),
                    );
                    let h = self.w.add(
                        HeaderFacts {
                            number: parent.number + 1,
                            hash,
                            parent: parent.hash,
                            round: r,
                            qc: Some(qc.clone()),
                        },
                        id,
                    );
                    let msg = Arc::new(Msg::Block(h, id));
                    for &to in &honest {
                        if self.rng.pct(70) {
                            self.send(id, to, msg.clone());
                        }
                    }
                }
            }
            // a SyncInfo that does not verify
            7 => {
                let to = honest[self.rng.below(honest.len() as u64) as usize];
                let Some(top) = self
                    .w
                    .blocks
                    .values()
                    .max_by_key(|h| (h.round, h.hash))
                    .cloned()
                else {
                    return;
                };
                let info = BlockInfo {
                    hash: top.hash,
                    round: top.round + self.rng.below(3),
                    number: top.number,
                };
                let d = vote_digest(&info, 0);
                let mut sigs: Vec<Bytes> = (0..1 + self.rng.below(self.sc.need as u64 - 1))
                    .map(|k| sign(&me, &d, k))
                    .collect();
                match self.rng.below(3) {
                    0 => {}
                    1 => sigs.extend((0..self.sc.n).map(|i| {
                        sign(&addr(i), &d, 99)
                            .iter()
                            .cloned()
                            .take(20)
                            .chain([7u8; 44])
                            .collect::<Vec<u8>>()
                            .into()
                    })),
                    _ => sigs = vec![Bytes::from(vec![3u8; 60]); 151],
                }
                let tc_round = top.round + 1 + self.rng.below(20);
                let tc = TimeoutCert {
                    round: tc_round,
                    signatures: vec![sign(&me, &timeout_digest(tc_round, 0), 0)],
                    gap_number: 0,
                };
                let si = SyncInfo {
                    highest_quorum_cert: QuorumCert {
                        proposed_block_info: info,
                        signatures: sigs,
                        gap_number: 0,
                    },
                    highest_timeout_cert: tc,
                };
                self.stats.byz_forged_sync += 1;
                self.send(id, to, Arc::new(Msg::Sync(si)));
            }
            _ => {}
        }
    }

    fn run(mut self) -> Result<Report, String> {
        for i in 0..self.sc.n {
            self.push(1 + i as u64, Ev::SyncTick { node: i });
            if self.nodes[i].byz {
                self.push(1, Ev::ByzTick { node: i });
            } else {
                self.start(i)?;
            }
        }
        for (t, node) in self.sc.kills.clone() {
            self.push(t, Ev::Kill { node });
        }
        let end = self.sc.gst + self.sc.bound + 10;
        while let Some(((t, _), ev)) = self.q.pop_first() {
            if t > end {
                break;
            }
            if self.base.is_none() && t >= self.sc.gst {
                self.base = Some(
                    self.nodes
                        .iter()
                        .filter(|n| !n.byz)
                        .map(|n| n.committed)
                        .max()
                        .unwrap_or(0),
                );
                if self.recovery.is_none()
                    && self
                        .honest()
                        .all(|i| self.nodes[i].committed > self.base.unwrap_or(0))
                {
                    self.recovery = Some(0);
                }
            }
            self.t = t;
            match ev {
                Ev::Deliver { to, from, msg } => self.deliver(to, from, msg)?,
                Ev::Timer { node, token } => self.step(node, Event::Timer(token))?,
                Ev::Propose { node, round } => self.propose(node, round)?,
                Ev::Kill { node } => {
                    if !self.nodes[node].byz && self.nodes[node].bft.is_some() {
                        self.die(node)?;
                    }
                }
                Ev::Restart { node } => {
                    if self.nodes[node].bft.is_none() {
                        let keep = self.rng.pct(50);
                        self.nodes[node].store.power_cycle(keep);
                        self.start(node)?;
                    }
                }
                Ev::SyncTick { node } => self.sync_tick(node),
                Ev::ByzTick { node } => self.byz_tick(node),
            }
        }
        for i in 0..self.sc.n {
            if !self.nodes[i].byz && self.nodes[i].bft.is_none() {
                return Err(format!("node {i} is still down at the end"));
            }
        }
        let report = Report {
            n: self.sc.n,
            byz: self.sc.byz.len(),
            gst: self.sc.gst,
            bound: self.sc.bound,
            recovery: self.recovery,
            stats: self.stats.clone(),
        };
        match self.recovery {
            Some(r) if r <= self.sc.bound => Ok(report),
            r => Err(format!(
                "no recovery within the bound: {r:?} of {} ticks after GST {} (n {}, byz {:?}, T max {}, delta {}, rows {:?})",
                self.sc.bound, self.sc.gst, self.sc.n, self.sc.byz, self.sc.t_max, self.sc.delta_post,
                self.sc.rows.iter().map(|(k, r)| (*k, r.timeout_period, r.exp_timeout.base, r.exp_timeout.max_exponent, r.timeout_sync_threshold)).collect::<Vec<_>>()
            )),
        }
    }
}

/// Run one seed. `Err` names the violation or the missed recovery bound.
pub fn run(seed: u64) -> Result<Report, String> {
    Sim::new(seed)
        .run()
        .map_err(|e| format!("seed {seed}: {e}"))
}

/// `run` that prints the events of the run on stderr.
pub fn run_traced(seed: u64) -> Result<Report, String> {
    let mut s = Sim::new(seed);
    s.trace = true;
    eprintln!("{:?}", s.sc);
    s.run().map_err(|e| format!("seed {seed}: {e}"))
}

/// The scenario of a seed, for the README and the tests.
pub fn scenario(seed: u64) -> Scenario {
    Scenario::new(seed)
}

/// Used by the tests to tell the Duration of a timer from a tick count.
pub fn ticks(d: Duration) -> u64 {
    d.as_secs_f64().ceil() as u64
}
