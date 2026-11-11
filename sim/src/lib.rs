//! Deterministic simulator of one validator's V2 vote path (ADR-010). Test-only.
//! One seed gives one run. Signatures are stand-ins keyed by (kind, round, digest).
//!
//! Oracle: every signature that left the node. A run fails if two signatures of one kind and
//! round have different digests, or a new vote is emitted for a round at or below an earlier
//! timeout. The record must also never go down across a restart (except a stale restore).
//!
//! Model limits (ADR-010 section 9): a signature that left the node reaches the network, so a
//! restored node sees it during its quiet window. A signature that reached nobody is not
//! covered by any local mechanism; `unseen_signature_is_the_residual_risk` shows it.

#![forbid(unsafe_code)]

pub mod net;

use alloy_primitives::{keccak256, Address, B256};
use xone_consensus::v2::safety::{decide, BlockRef, Fence, Intent, Kind, Refusal, SafetyRecord};
use xone_db::safety::{Fault, MemStore, SafetyStore};
use xone_db::{Cause, Durability};

const STEPS: u32 = 100;
const QUIET: u32 = 3;

pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub(crate) fn pct(&mut self, p: u64) -> bool {
        self.below(100) < p
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mutant {
    None,
    /// Sign without writing the intent first.
    NoPersist,
    /// No quiet window and no sightings after a restore.
    NoFence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kill {
    BeforePersist,
    WriteBeforeSync,
    FsyncError,
    SyncBeforeAck,
    AfterAck,
    AfterSign,
    AfterSend,
}

const KILLS: [Kill; 7] = [
    Kill::BeforePersist,
    Kill::WriteBeforeSync,
    Kill::FsyncError,
    Kill::SyncBeforeAck,
    Kill::AfterAck,
    Kill::AfterSign,
    Kill::AfterSend,
];

/// Counters used to prove a run is not vacuous.
#[derive(Default, Debug, Clone)]
pub struct Stats {
    pub emitted: u64,
    pub kills: [u64; 7],
    pub stale_restores: u64,
    pub conflict: u64,
    pub timed_out: u64,
    pub lock: u64,
    pub below: u64,
    pub degraded: u64,
    pub replays: u64,
}

const ID: Address = Address::repeat_byte(0x11);

fn digest(round: u64, n: u64) -> B256 {
    keccak256([&round.to_le_bytes()[..], &n.to_le_bytes()].concat())
}

/// Stand-in for the signer: deterministic, so a retry gives the same signature.
fn sign(i: &Intent) -> B256 {
    keccak256(
        [
            &[i.kind as u8][..],
            &i.round.to_le_bytes(),
            i.digest.as_slice(),
        ]
        .concat(),
    )
}

struct Sim {
    rng: Rng,
    mutant: Mutant,
    store: MemStore,
    /// The running process' copy.
    rec: SafetyRecord,
    /// Last record the store acknowledged or a restart loaded.
    acked: SafetyRecord,
    fence: Fence,
    alive: bool,
    sight_pending: bool,
    partitioned: bool,
    degraded: bool,
    net_round: u64,
    /// The oracle: (kind, round, digest, signature) in emission order.
    emitted: Vec<(Kind, u64, B256, B256)>,
    sent: Vec<Intent>,
    retry: Option<Intent>,
    stats: Stats,
}

impl Sim {
    fn new(seed: u64, mutant: Mutant) -> Self {
        let rec = SafetyRecord::new(ID);
        Self {
            rng: Rng(seed),
            mutant,
            store: MemStore::default(),
            acked: rec.clone(),
            rec,
            fence: Fence::start(0),
            alive: true,
            sight_pending: false,
            partitioned: false,
            degraded: false,
            net_round: 1,
            emitted: vec![],
            sent: vec![],
            retry: None,
            stats: Stats::default(),
        }
    }

    fn quiet(&self) -> u32 {
        if self.mutant == Mutant::NoFence {
            0
        } else {
            QUIET
        }
    }

    fn die(&mut self, k: Kill) {
        self.stats.kills[KILLS.iter().position(|x| *x == k).unwrap()] += 1;
        self.alive = false;
    }

    /// The signature leaves the node. Checks the oracle.
    fn emit(&mut self, i: &Intent) -> Result<(), String> {
        let sig = sign(i);
        let key = (i.kind, i.round, i.digest, sig);
        if self.emitted.contains(&key) {
            return Ok(());
        }
        for e in &self.emitted {
            if e.0 == i.kind && e.1 == i.round {
                return Err(format!(
                    "equivocation: {:?} round {} two digests",
                    i.kind, i.round
                ));
            }
            if i.kind == Kind::Vote && e.0 == Kind::Timeout && e.1 >= i.round {
                return Err(format!(
                    "vote round {} after timeout round {}",
                    i.round, e.1
                ));
            }
        }
        self.emitted.push(key);
        self.stats.emitted += 1;
        Ok(())
    }

    /// Vote path: fence, decide, persist, ack, sign, send.
    fn request(&mut self, i: Intent, kill: Option<Kill>) -> Result<(), String> {
        self.sent.push(i);
        if !self.alive {
            return Ok(());
        }
        if !self.fence.open() {
            self.retry = Some(i);
            return Ok(());
        }
        let dur = if self.degraded {
            Durability::Degraded(Cause::NoSpace)
        } else {
            Durability::Durable
        };
        let next = match decide(&self.rec, &i, dur) {
            Ok(n) => n,
            Err(r) => {
                match r {
                    Refusal::Conflict { .. } => self.stats.conflict += 1,
                    Refusal::TimedOut { .. } => self.stats.timed_out += 1,
                    Refusal::Lock { .. } => self.stats.lock += 1,
                    Refusal::BelowBound { .. } => self.stats.below += 1,
                    Refusal::Degraded(_) => self.stats.degraded += 1,
                    Refusal::RoundZero => {}
                }
                return Ok(());
            }
        };
        if next == self.rec {
            self.stats.replays += 1;
        } else {
            if self.mutant != Mutant::NoPersist {
                if kill == Some(Kill::BeforePersist) {
                    self.retry = Some(i);
                    self.die(Kill::BeforePersist);
                    return Ok(());
                }
                match kill {
                    Some(Kill::WriteBeforeSync) => self.store.arm(Fault::AfterWrite),
                    Some(Kill::FsyncError) => self.store.arm(Fault::FailSync),
                    Some(Kill::SyncBeforeAck) => self.store.arm(Fault::AfterSync),
                    _ => {}
                }
                if self
                    .store
                    .persist_intent(&i.to_bytes(), &next.encode())
                    .is_err()
                {
                    // Any store error is fail-stop: no signing until restart.
                    self.retry = Some(i);
                    self.die(kill.unwrap_or(Kill::FsyncError));
                    return Ok(());
                }
                self.acked = next.clone();
            }
            self.rec = next;
        }
        self.store.disarm();
        let sig_ready = sign(&i);
        if kill == Some(Kill::AfterAck) {
            self.retry = Some(i);
            self.die(Kill::AfterAck);
            return Ok(());
        }
        if kill == Some(Kill::AfterSign) {
            self.retry = Some(i);
            self.die(Kill::AfterSign);
            return Ok(());
        }
        debug_assert_eq!(sig_ready, sign(&i));
        self.emit(&i)?;
        if kill == Some(Kill::AfterSend) {
            self.die(Kill::AfterSend);
        }
        Ok(())
    }

    fn restart(&mut self, stale: bool) -> Result<(), String> {
        let keep = self.rng.pct(50);
        self.store.power_cycle(keep);
        if stale {
            let n = self.store.history_len();
            let pick = self.rng.below(n as u64 + 1) as usize;
            self.store.restore(pick.checked_sub(1).filter(|_| pick > 0));
            self.stats.stale_restores += 1;
            self.sight_pending = true;
        }
        let loaded = self.store.load().map_err(|e| format!("load: {e}"))?;
        self.rec = match loaded {
            Some(l) => SafetyRecord::decode(&l.record).map_err(|e| format!("decode: {e}"))?,
            None => SafetyRecord::new(ID),
        };
        if self.mutant == Mutant::None && !stale && !self.rec.dominates(&self.acked) {
            return Err("record went down across a restart".into());
        }
        self.acked = self.rec.clone();
        self.fence = Fence::start(self.quiet());
        self.alive = true;
        Ok(())
    }

    /// First connected tick after a restore: our signatures seen on the network raise the record.
    fn apply_sightings(&mut self) {
        if self.mutant == Mutant::NoFence {
            return;
        }
        let mut last = None;
        for &(k, r, d, _) in &self.emitted {
            if self.rec.sight(k, r, d) {
                last = Some(Intent::new(k, r, d));
            }
        }
        if let Some(i) = last {
            self.store
                .persist_intent(&i.to_bytes(), &self.rec.encode())
                .expect("mem store without armed fault");
            self.acked = self.rec.clone();
            self.fence.sighted();
        }
    }

    fn near_round(&mut self) -> u64 {
        match self.rng.below(10) {
            0 => self.net_round.saturating_sub(1).max(1),
            1 => self.net_round + 1,
            _ => self.net_round,
        }
    }

    fn arm_kill(&mut self) -> Option<Kill> {
        if self.rng.pct(25) {
            Some(KILLS[self.rng.below(KILLS.len() as u64) as usize])
        } else {
            None
        }
    }

    fn step(&mut self) -> Result<(), String> {
        if !self.alive {
            return self.restart(false);
        }
        match self.rng.below(100) {
            0..=29 => {
                let r = self.near_round();
                let n = self.rng.below(3);
                let qc = self.rng.below(r);
                let ext = self.rng.pct(50);
                let kill = self.arm_kill();
                if !self.partitioned {
                    self.request(Intent::vote(r, digest(r, n), qc, ext), kill)?;
                }
            }
            30..=41 => {
                let r = self.near_round();
                let gap = if self.rng.pct(10) { 1 } else { 0 };
                let kill = self.arm_kill();
                self.request(Intent::new(Kind::Timeout, r, digest(r, 100 + gap)), kill)?;
            }
            42..=49 => {
                let r = self.net_round;
                let n = self.rng.below(3);
                let kill = self.arm_kill();
                self.request(Intent::new(Kind::Propose, r, digest(r, 200 + n)), kill)?;
            }
            50..=57 => {
                self.net_round += 1;
                if !self.partitioned {
                    let r = self.net_round;
                    let lock_round = r.saturating_sub(1 + self.rng.below(3)).max(1);
                    let qc = BlockRef {
                        hash: digest(r, 0),
                        round: r,
                        number: r * 10,
                    };
                    let lock = BlockRef {
                        hash: digest(lock_round, 0),
                        round: lock_round,
                        number: lock_round * 10,
                    };
                    self.rec.observe_qc(qc, self.rng.pct(70).then_some(lock));
                }
            }
            58..=63 => {
                self.net_round += 1;
                if !self.partitioned {
                    self.rec.observe_tc(self.net_round);
                }
            }
            64..=83 => {
                if self.rng.pct(40) {
                    self.degraded = false;
                }
                if self.rng.pct(20) {
                    self.partitioned = false;
                }
                if !self.partitioned {
                    if self.sight_pending {
                        self.apply_sightings();
                        self.sight_pending = false;
                    } else {
                        self.fence.tick();
                    }
                    if self.fence.open() {
                        if let Some(i) = self.retry.take() {
                            self.request(i, None)?;
                        }
                    }
                }
            }
            84..=85 => self.alive = false,
            86..=87 => self.restart(true)?,
            88..=89 => self.partitioned = true,
            90..=92 => self.degraded = true,
            _ => {
                if !self.sent.is_empty() {
                    let i = self.sent[self.rng.below(self.sent.len() as u64) as usize];
                    self.request(i, None)?;
                }
            }
        }
        Ok(())
    }
}

/// Run one seed. `Err` names the violation.
pub fn run(seed: u64, mutant: Mutant) -> Result<Stats, String> {
    let mut s = Sim::new(seed, mutant);
    for _ in 0..STEPS {
        s.step()?;
    }
    Ok(s.stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// The release run is the gate (`cargo test --release`); debug runs a smoke subset.
    const SEEDS: u64 = if cfg!(debug_assertions) {
        1_000
    } else {
        20_000
    };

    impl Stats {
        fn add(&mut self, o: &Stats) {
            self.emitted += o.emitted;
            for (a, b) in self.kills.iter_mut().zip(o.kills) {
                *a += b;
            }
            self.stale_restores += o.stale_restores;
            self.conflict += o.conflict;
            self.timed_out += o.timed_out;
            self.lock += o.lock;
            self.below += o.below;
            self.degraded += o.degraded;
            self.replays += o.replays;
        }
    }

    #[test]
    fn no_conflicting_signature_in_any_seed() {
        let t = Instant::now();
        let mut total = Stats::default();
        for seed in 0..SEEDS {
            match run(seed, Mutant::None) {
                Ok(s) => total.add(&s),
                Err(e) => panic!("seed {seed}: {e}"),
            }
        }
        eprintln!("{SEEDS} seeds in {:?}: {total:?}", t.elapsed());
        assert!(total.emitted > 0 && total.stale_restores > 0 && total.replays > 0);
        assert!(
            total.conflict > 0
                && total.timed_out > 0
                && total.lock > 0
                && total.below > 0
                && total.degraded > 0
        );
        assert!(
            total.kills.iter().all(|k| *k > 0),
            "every kill point was hit: {:?}",
            total.kills
        );
    }

    /// First seed in which the mutant is caught.
    fn first_failure(m: Mutant) -> Option<(u64, String)> {
        (0..SEEDS).find_map(|seed| run(seed, m).err().map(|e| (seed, e)))
    }

    #[test]
    fn mutant_without_persist_before_sign_equivocates() {
        let first = first_failure(Mutant::NoPersist);
        eprintln!("NoPersist caught: {first:?}");
        assert!(first.is_some());
    }

    #[test]
    fn mutant_without_fence_equivocates_after_stale_restore() {
        let first = first_failure(Mutant::NoFence);
        eprintln!("NoFence caught: {first:?}");
        assert!(first.is_some());
    }

    /// The limit of the fence: a signature seen by nobody leaves no trace to restore from.
    #[test]
    fn unseen_signature_is_the_residual_risk() {
        let old = SafetyRecord::new(ID);
        let a = Intent::vote(5, digest(5, 0), 4, true);
        let b = Intent::vote(5, digest(5, 1), 4, true);
        let after_a = decide(&old, &a, Durability::Durable).unwrap();
        assert!(decide(&after_a, &b, Durability::Durable).is_err());
        // Restore the record from before `a`, with no sighting of `a`: `b` is accepted.
        assert!(decide(&old, &b, Durability::Durable).is_ok());
    }
}
