//! XDPoS V2 signer safety (ADR-010): the pure decision and the record bytes.
//! No I/O, no clock, no keys. Rule sources are cited as geth-XDC file:line in the ADR.

use alloy_primitives::keccak256;
use xone_db::{Address, Cause, Durability, B256};

/// What a signature is for. The digest of a timeout is its sig hash (round and gap).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Vote = 1,
    Timeout = 2,
    Propose = 3,
    /// A V1 block seal (M1) or co-signature (M2), keyed by block number: its own slot, because V2
    /// rounds and V1 heights are different counters and a height never bounds a round.
    SealV1 = 4,
}

impl Kind {
    fn idx(self) -> usize {
        #[allow(clippy::as_conversions)] // fieldless enum discriminant (1..=3)
        let d = self as usize;
        d - 1
    }
}

/// Facts about a proposal that only the consensus step can compute from the chain.
/// Used by the voting rule only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VoteCtx {
    /// Round of the QC the proposal carries.
    pub qc_round: u64,
    /// The proposal extends the block of the lock QC.
    pub extends_lock: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Intent {
    pub kind: Kind,
    pub round: u64,
    pub digest: B256,
    pub ctx: VoteCtx,
}

impl Intent {
    pub fn new(kind: Kind, round: u64, digest: B256) -> Self {
        Self {
            kind,
            round,
            digest,
            ctx: VoteCtx::default(),
        }
    }

    pub fn vote(round: u64, digest: B256, qc_round: u64, extends_lock: bool) -> Self {
        Self {
            ctx: VoteCtx {
                qc_round,
                extends_lock,
            },
            ..Self::new(Kind::Vote, round, digest)
        }
    }

    /// `kind u8 | round u64 LE | digest 32`. The context is not stored: a retry is matched on these 41 bytes.
    pub fn to_bytes(&self) -> [u8; 41] {
        let mut b = [0; 41];
        #[allow(clippy::as_conversions)] // fieldless enum discriminant (1..=3)
        let kind = self.kind as u8;
        b[0] = kind;
        b[1..9].copy_from_slice(&self.round.to_le_bytes());
        b[9..].copy_from_slice(self.digest.as_slice());
        b
    }
}

/// The highest signature of one kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slot {
    pub round: u64,
    pub digest: B256,
}

/// Block reference of a QC. Same fields as `BlockInfo` of XB-04; swap when that lands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockRef {
    pub hash: B256,
    pub round: u64,
    pub number: u64,
}

/// Non-reversible signing state. Every field only grows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyRecord {
    pub identity: Address,
    /// Writes of this record. Not a clock.
    pub seq: u64,
    /// Highest round of a vote or a timeout. A vote needs a round above it.
    pub highest_voted_round: u64,
    /// Highest signed round and digest, indexed by `Kind` (vote, timeout, propose, V1 seal).
    pub signed: [Slot; 4],
    pub lock_qc: Option<BlockRef>,
    pub highest_qc: BlockRef,
    pub highest_tc: u64,
}

pub const RECORD_LEN: usize = 339;
const MAGIC: &[u8; 4] = b"XSR1";
const VERSION: u16 = 2;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("record length {0}, expected {RECORD_LEN}")]
    Length(usize),
    #[error("bad magic")]
    Magic,
    #[error("format version {0}")]
    Version(u16),
    #[error("checksum mismatch")]
    Checksum,
    #[error("non-canonical lock flag or padding")]
    Layout,
}

impl SafetyRecord {
    pub fn new(identity: Address) -> Self {
        Self {
            identity,
            seq: 0,
            highest_voted_round: 0,
            signed: [Slot::default(); 4],
            lock_qc: None,
            highest_qc: BlockRef::default(),
            highest_tc: 0,
        }
    }

    /// All integers little endian. Layout (339 bytes):
    /// magic 4 | version 2 | identity 20 | seq 8 | highest_voted_round 8 |
    /// 4 x (round 8 | digest 32) | lock flag 1 | lock QC 48 | highest QC 48 |
    /// highest_tc 8 | keccak256 of the preceding 307 bytes.
    /// A QC is `hash 32 | round 8 | number 8`.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(RECORD_LEN);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(self.identity.as_slice());
        b.extend_from_slice(&self.seq.to_le_bytes());
        b.extend_from_slice(&self.highest_voted_round.to_le_bytes());
        for s in &self.signed {
            b.extend_from_slice(&s.round.to_le_bytes());
            b.extend_from_slice(s.digest.as_slice());
        }
        b.push(u8::from(self.lock_qc.is_some()));
        // absent lock encodes as an all-zero ref after the flag byte; the flag disambiguates
        #[allow(clippy::disallowed_methods)]
        let lock = self.lock_qc.unwrap_or_default();
        put_ref(&mut b, &lock);
        put_ref(&mut b, &self.highest_qc);
        b.extend_from_slice(&self.highest_tc.to_le_bytes());
        let sum = keccak256(&b);
        b.extend_from_slice(sum.as_slice());
        b
    }

    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        if b.len() != RECORD_LEN {
            return Err(DecodeError::Length(b.len()));
        }
        let (body, sum) = b.split_at(RECORD_LEN - 32);
        if &body[..4] != MAGIC {
            return Err(DecodeError::Magic);
        }
        let version = u16::from_le_bytes([body[4], body[5]]);
        if version != VERSION {
            return Err(DecodeError::Version(version));
        }
        if keccak256(body).as_slice() != sum {
            return Err(DecodeError::Checksum);
        }
        let mut r = Reader(&body[6..]);
        let identity = Address::from_slice(r.take(20));
        let seq = r.u64();
        let highest_voted_round = r.u64();
        let mut signed = [Slot::default(); 4];
        for s in &mut signed {
            *s = Slot {
                round: r.u64(),
                digest: r.b256(),
            };
        }
        let flag = r.take(1)[0];
        let lock = r.block_ref();
        let lock_qc = match (flag, lock == BlockRef::default()) {
            (1, _) => Some(lock),
            (0, true) => None,
            _ => return Err(DecodeError::Layout),
        };
        let highest_qc = r.block_ref();
        let highest_tc = r.u64();
        Ok(Self {
            identity,
            seq,
            highest_voted_round,
            signed,
            lock_qc,
            highest_qc,
            highest_tc,
        })
    }

    /// A QC was seen. Mirrors geth `processQC`: highest QC by round (utils.go:480-486), lock
    /// replaced by the QC carried in the certified block when its round is higher (utils.go:505-506).
    /// Returns true if the record changed.
    pub fn observe_qc(&mut self, qc: BlockRef, lock_candidate: Option<BlockRef>) -> bool {
        let mut changed = false;
        if qc.round > self.highest_qc.round {
            self.highest_qc = qc;
            changed = true;
        }
        if let Some(c) = lock_candidate {
            if self.lock_qc.is_none_or(|l| c.round > l.round) {
                self.lock_qc = Some(c);
                changed = true;
            }
        }
        self.seq += u64::from(changed);
        changed
    }

    /// A TC was seen. Mirrors geth `processTC` (timeout.go:476-478).
    pub fn observe_tc(&mut self, round: u64) -> bool {
        let changed = round > self.highest_tc;
        if changed {
            self.highest_tc = round;
            self.seq += 1;
        }
        changed
    }

    /// A signature of our key was seen on the network. Raises the record if it is above it.
    /// Returns true if the sighting is not explained by the record (stale record or a second signer).
    pub fn sight(&mut self, kind: Kind, round: u64, digest: B256) -> bool {
        let slot = self.signed[kind.idx()];
        if round < slot.round || (round == slot.round && digest == slot.digest) {
            return false;
        }
        if round > slot.round {
            self.signed[kind.idx()] = Slot { round, digest };
            if matches!(kind, Kind::Vote | Kind::Timeout) {
                self.highest_voted_round = self.highest_voted_round.max(round);
            }
            self.seq += 1;
        }
        true
    }

    /// No field of `self` is below the same field of `old`.
    pub fn dominates(&self, old: &Self) -> bool {
        self.identity == old.identity
            && self.seq >= old.seq
            && self.highest_voted_round >= old.highest_voted_round
            && self
                .signed
                .iter()
                .zip(&old.signed)
                .all(|(a, b)| a.round >= b.round)
            && self.lock_qc.map(|l| l.round) >= old.lock_qc.map(|l| l.round)
            && self.highest_qc.round >= old.highest_qc.round
            && self.highest_tc >= old.highest_tc
    }
}

impl SafetyRecord {
    /// The fieldwise maximum of two records of one identity: the record that holds everything either
    /// holds. The machine's record (its observations) and the unit's (every signature, proposals
    /// included) are joined before a write, so neither lowers the other. `seq` counts the writes.
    pub fn join(&self, o: &Self) -> Self {
        let by_round = |a: Option<BlockRef>, b: Option<BlockRef>| match (a, b) {
            (Some(x), Some(y)) => Some(if y.round > x.round { y } else { x }),
            (x, None) => x,
            (None, y) => y,
        };
        let mut j = self.clone();
        j.highest_voted_round = j.highest_voted_round.max(o.highest_voted_round);
        for (a, b) in j.signed.iter_mut().zip(&o.signed) {
            if b.round > a.round {
                *a = *b;
            }
        }
        j.lock_qc = by_round(j.lock_qc, o.lock_qc);
        if o.highest_qc.round > j.highest_qc.round {
            j.highest_qc = o.highest_qc;
        }
        j.highest_tc = j.highest_tc.max(o.highest_tc);
        j.seq = if j == *self {
            self.seq
        } else {
            self.seq.max(o.seq) + 1
        };
        j
    }
}

fn put_ref(b: &mut Vec<u8>, r: &BlockRef) {
    b.extend_from_slice(r.hash.as_slice());
    b.extend_from_slice(&r.round.to_le_bytes());
    b.extend_from_slice(&r.number.to_le_bytes());
}

/// The caller checked the total length, so a short read cannot happen.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> &[u8] {
        let (h, t) = self.0.split_at(n);
        self.0 = t;
        h
    }
    fn u64(&mut self) -> u64 {
        // take(8) is exactly 8 bytes (caller checked the total length), so the default is unreachable
        #[allow(clippy::disallowed_methods)]
        let b = self.take(8).try_into().unwrap_or_default();
        u64::from_le_bytes(b)
    }
    fn b256(&mut self) -> B256 {
        B256::from_slice(self.take(32))
    }
    fn block_ref(&mut self) -> BlockRef {
        BlockRef {
            hash: self.b256(),
            round: self.u64(),
            number: self.u64(),
        }
    }
}

/// Why no signature may be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("storage cannot guarantee durability: {0:?}")]
    Degraded(Cause),
    #[error("round 0 is never signed")]
    RoundZero,
    /// One signature per kind and round: this round already has another digest.
    #[error("{kind:?} round {round} already signed with another digest")]
    Conflict { kind: Kind, round: u64 },
    #[error("{kind:?} round {round} is not above the signed round {bound}")]
    BelowBound { kind: Kind, round: u64, bound: u64 },
    /// A timeout for this round or a later one was signed.
    #[error("vote for round {round} after a vote or timeout at round {bound}")]
    TimedOut { round: u64, bound: u64 },
    #[error("proposal does not extend the lock (QC round {qc_round} <= lock round {lock_round})")]
    Lock { qc_round: u64, lock_round: u64 },
}

/// May this intent be signed? On success returns the record to persist before signing.
/// A returned record equal to the input means the intent is already in the record:
/// a retry. It must give the same signature (deterministic signer) and needs no write.
pub fn decide(
    rec: &SafetyRecord,
    i: &Intent,
    durability: Durability,
) -> Result<SafetyRecord, Refusal> {
    if let Durability::Degraded(c) = durability {
        return Err(Refusal::Degraded(c));
    }
    // Round 0 is the initial value of every guard (geth engine.go:235, vote.go:406).
    if i.round == 0 {
        return Err(Refusal::RoundZero);
    }
    let slot = rec.signed[i.kind.idx()];
    if slot.round == i.round {
        // A retry. A vote that was authorised but never sent is not released after a timeout
        // for the same or a later round was signed (ADR-010 section 7).
        let t = rec.signed[Kind::Timeout.idx()].round;
        if i.kind == Kind::Vote && slot.digest == i.digest && t >= i.round {
            return Err(Refusal::TimedOut {
                round: i.round,
                bound: t,
            });
        }
        return if slot.digest == i.digest {
            Ok(rec.clone())
        } else {
            Err(Refusal::Conflict {
                kind: i.kind,
                round: i.round,
            })
        };
    }
    if i.round < slot.round {
        return Err(Refusal::BelowBound {
            kind: i.kind,
            round: i.round,
            bound: slot.round,
        });
    }
    match i.kind {
        Kind::Vote => {
            // vote.go:406-411: no vote for a round <= highest voted. The bound also holds timeouts.
            if i.round <= rec.highest_voted_round {
                return Err(Refusal::TimedOut {
                    round: i.round,
                    bound: rec.highest_voted_round,
                });
            }
            // vote.go:422-443: vote if the QC is above the lock or the proposal extends the lock.
            if let Some(l) = rec.lock_qc {
                if !(i.ctx.qc_round > l.round || i.ctx.extends_lock) {
                    return Err(Refusal::Lock {
                        qc_round: i.ctx.qc_round,
                        lock_round: l.round,
                    });
                }
            }
        }
        // ADR-010 section 7: a timeout may not go below any vote or timeout round.
        Kind::Timeout if i.round < rec.highest_voted_round => {
            return Err(Refusal::BelowBound {
                kind: i.kind,
                round: i.round,
                bound: rec.highest_voted_round,
            });
        }
        // mining.go:26: one proposal per round, none below the highest.
        _ => {}
    }
    let mut next = rec.clone();
    next.seq += 1;
    next.signed[i.kind.idx()] = Slot {
        round: i.round,
        digest: i.digest,
    };
    if matches!(i.kind, Kind::Vote | Kind::Timeout) {
        next.highest_voted_round = i.round;
    }
    Ok(next)
}

/// Cross-host fence (ADR-010 section 8). `quiet` is the window in ticks of connected time.
/// The signer is closed from start until `quiet` ticks pass with no unexplained sighting of its key.
#[derive(Clone, Copy, Debug)]
pub struct Fence {
    quiet: u32,
    left: u32,
}

impl Fence {
    pub fn start(quiet: u32) -> Self {
        Self { quiet, left: quiet }
    }

    /// One tick of time in which valid gossip was received.
    pub fn tick(&mut self) {
        self.left = self.left.saturating_sub(1);
    }

    /// `SafetyRecord::sight` returned true: restart the window.
    pub fn sighted(&mut self) {
        self.left = self.quiet;
    }

    pub fn open(&self) -> bool {
        self.left == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: u8) -> B256 {
        B256::with_last_byte(n)
    }

    fn rec() -> SafetyRecord {
        SafetyRecord::new(Address::repeat_byte(7))
    }

    fn ok(r: &SafetyRecord, i: Intent) -> SafetyRecord {
        decide(r, &i, Durability::Durable).unwrap()
    }

    fn err(r: &SafetyRecord, i: Intent) -> Refusal {
        decide(r, &i, Durability::Durable).unwrap_err()
    }

    #[test]
    fn encoding_round_trip_and_rejects() {
        let mut r = rec();
        r = ok(&r, Intent::vote(5, d(1), 4, false));
        r = ok(&r, Intent::new(Kind::Timeout, 6, d(2)));
        r = ok(&r, Intent::new(Kind::Propose, 9, d(3)));
        r.observe_qc(
            BlockRef {
                hash: d(4),
                round: 8,
                number: 80,
            },
            Some(BlockRef {
                hash: d(5),
                round: 7,
                number: 79,
            }),
        );
        r.observe_tc(8);
        let b = r.encode();
        assert_eq!(b.len(), RECORD_LEN);
        assert_eq!(SafetyRecord::decode(&b), Ok(r.clone()));
        let empty = rec();
        assert_eq!(SafetyRecord::decode(&empty.encode()), Ok(empty));
        assert_eq!(SafetyRecord::decode(&b[1..]), Err(DecodeError::Length(338)));
        for i in 0..b.len() {
            let mut c = b.clone();
            c[i] ^= 1;
            assert!(SafetyRecord::decode(&c).is_err(), "byte {i}");
        }
        // Fixed bytes of a known record: the layout is part of the format.
        let mut known = rec();
        known.seq = 1;
        known.highest_voted_round = 2;
        let e = known.encode();
        assert_eq!(&e[..6], b"XSR1\x02\x00");
        assert_eq!(&e[6..26], &[7; 20]);
        assert_eq!(&e[26..34], &1u64.to_le_bytes());
        assert_eq!(&e[34..42], &2u64.to_le_bytes());
    }

    #[test]
    fn one_vote_per_round_and_replay() {
        let r = ok(&rec(), Intent::vote(5, d(1), 4, false));
        assert_eq!(r.highest_voted_round, 5);
        assert_eq!(
            ok(&r, Intent::vote(5, d(1), 4, false)),
            r,
            "replay writes nothing"
        );
        assert_eq!(
            err(&r, Intent::vote(5, d(2), 4, false)),
            Refusal::Conflict {
                kind: Kind::Vote,
                round: 5
            }
        );
        assert_eq!(
            err(&r, Intent::vote(4, d(2), 3, false)),
            Refusal::BelowBound {
                kind: Kind::Vote,
                round: 4,
                bound: 5
            }
        );
        assert_eq!(
            err(&rec(), Intent::vote(0, d(1), 0, true)),
            Refusal::RoundZero
        );
    }

    #[test]
    fn lock_rule() {
        let mut r = rec();
        r.observe_qc(
            BlockRef::default(),
            Some(BlockRef {
                hash: d(9),
                round: 10,
                number: 1,
            }),
        );
        assert_eq!(
            err(&r, Intent::vote(12, d(1), 10, false)),
            Refusal::Lock {
                qc_round: 10,
                lock_round: 10
            }
        );
        ok(&r, Intent::vote(12, d(1), 10, true));
        ok(&r, Intent::vote(12, d(1), 11, false));
        // geth utils.go:505: the lock only rises.
        r.observe_qc(
            BlockRef::default(),
            Some(BlockRef {
                hash: d(8),
                round: 3,
                number: 1,
            }),
        );
        assert_eq!(r.lock_qc.unwrap().round, 10);
    }

    #[test]
    fn timeout_rule() {
        let r = ok(&rec(), Intent::vote(5, d(1), 4, false));
        // vote then timeout in the same round is allowed; the timeout retry is a replay,
        // the vote retry is not (it would be emitted after the timeout).
        let t = ok(&r, Intent::new(Kind::Timeout, 5, d(2)));
        assert_eq!(ok(&t, Intent::new(Kind::Timeout, 5, d(2))), t);
        assert_eq!(
            err(&t, Intent::vote(5, d(1), 4, false)),
            Refusal::TimedOut { round: 5, bound: 5 }
        );
        assert_eq!(
            err(&t, Intent::new(Kind::Timeout, 5, d(3))),
            Refusal::Conflict {
                kind: Kind::Timeout,
                round: 5
            }
        );
        // timeout first, then no vote for that round or below.
        let t = ok(&rec(), Intent::new(Kind::Timeout, 7, d(2)));
        assert_eq!(t.highest_voted_round, 7);
        assert_eq!(
            err(&t, Intent::vote(7, d(1), 6, true)),
            Refusal::TimedOut { round: 7, bound: 7 }
        );
        assert_eq!(
            err(&t, Intent::vote(6, d(1), 5, true)),
            Refusal::TimedOut { round: 6, bound: 7 }
        );
        ok(&t, Intent::vote(8, d(1), 7, true));
        // no timeout below a voted round.
        let v = ok(&rec(), Intent::vote(9, d(1), 8, true));
        assert_eq!(
            err(&v, Intent::new(Kind::Timeout, 8, d(2))),
            Refusal::BelowBound {
                kind: Kind::Timeout,
                round: 8,
                bound: 9
            }
        );
    }

    #[test]
    fn propose_rule_is_independent_of_timeouts() {
        let t = ok(&rec(), Intent::new(Kind::Timeout, 7, d(2)));
        let p = ok(&t, Intent::new(Kind::Propose, 7, d(3)));
        assert_eq!(p.highest_voted_round, 7);
        assert_eq!(
            err(&p, Intent::new(Kind::Propose, 7, d(4))),
            Refusal::Conflict {
                kind: Kind::Propose,
                round: 7
            }
        );
        assert_eq!(
            err(&p, Intent::new(Kind::Propose, 6, d(4))),
            Refusal::BelowBound {
                kind: Kind::Propose,
                round: 6,
                bound: 7
            }
        );
    }

    #[test]
    fn degraded_storage_blocks_every_kind_even_a_replay() {
        let r = ok(&rec(), Intent::vote(5, d(1), 4, false));
        for c in [
            Cause::Reconciling,
            Cause::NoSpace,
            Cause::Io,
            Cause::FsyncFailed,
        ] {
            for k in [Kind::Vote, Kind::Timeout, Kind::Propose, Kind::SealV1] {
                assert_eq!(
                    decide(&r, &Intent::new(k, 5, d(1)), Durability::Degraded(c)),
                    Err(Refusal::Degraded(c))
                );
            }
        }
    }

    #[test]
    fn a_v1_seal_has_its_own_slot_and_never_bounds_a_round() {
        let r = ok(&rec(), Intent::new(Kind::SealV1, 2700, d(1)));
        assert_eq!(
            r.signed[3],
            Slot {
                round: 2700,
                digest: d(1)
            }
        );
        assert_eq!(r.highest_voted_round, 0, "a height is not a voted round");
        // the same height with another digest is refused; the first V2 round is not
        assert_eq!(
            err(&r, Intent::new(Kind::SealV1, 2700, d(2))),
            Refusal::Conflict {
                kind: Kind::SealV1,
                round: 2700
            }
        );
        let r = ok(&r, Intent::new(Kind::Propose, 1, d(3)));
        assert_eq!((r.signed[2].round, r.signed[3].round), (1, 2700));
        let r = ok(&r, Intent::vote(1, d(4), 0, true));
        assert_eq!(r.highest_voted_round, 1);
    }

    #[test]
    fn join_holds_what_either_record_holds() {
        let a = ok(&rec(), Intent::new(Kind::Propose, 9, d(1)));
        let mut b = ok(&rec(), Intent::vote(5, d(2), 4, false));
        b.observe_tc(8);
        let j = a.join(&b);
        assert!(j.dominates(&a) && j.dominates(&b));
        assert_eq!(
            (j.signed[2].round, j.signed[0].round, j.highest_tc),
            (9, 5, 8)
        );
        assert_eq!(
            j.join(&a),
            j,
            "joining what it already holds changes nothing"
        );
        assert_eq!(j.join(&b).seq, j.seq);
    }

    #[test]
    fn decisions_only_raise_the_record() {
        let mut r = rec();
        for n in 1..40u64 {
            let kind = [Kind::Vote, Kind::Timeout, Kind::Propose][usize::try_from(n % 3).unwrap()];
            let i = Intent {
                ctx: VoteCtx {
                    qc_round: n,
                    extends_lock: true,
                },
                ..Intent::new(kind, n / 2 + 1, d(u8::try_from(n % 4).unwrap()))
            };
            if let Ok(next) = decide(&r, &i, Durability::Durable) {
                assert!(next.dominates(&r));
                r = next;
            }
        }
        assert!(r.seq > 0);
    }

    #[test]
    fn sighting_raises_and_restarts_the_fence() {
        let mut r = rec();
        let mut f = Fence::start(3);
        assert!(!f.open());
        for _ in 0..3 {
            f.tick();
        }
        assert!(f.open());
        assert!(r.sight(Kind::Vote, 9, d(1)));
        f.sighted();
        assert!(!f.open());
        assert_eq!(r.highest_voted_round, 9);
        assert!(!r.sight(Kind::Vote, 9, d(1)), "explained by the record");
        assert!(!r.sight(Kind::Vote, 4, d(2)));
        assert!(r.sight(Kind::Vote, 9, d(2)), "same round, other digest");
        // the raised record refuses what the stale one would have signed.
        assert_eq!(
            err(&r, Intent::vote(9, d(2), 8, true)),
            Refusal::Conflict {
                kind: Kind::Vote,
                round: 9
            }
        );
        assert_eq!(
            err(&r, Intent::vote(8, d(2), 7, true)),
            Refusal::BelowBound {
                kind: Kind::Vote,
                round: 8,
                bound: 9
            }
        );
    }
}
