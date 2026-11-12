//! The active V2 protocol as a deterministic step machine (plan #1 R3.4, issue #35): votes and
//! timeouts in, certificates and rounds out, every signature behind the safety unit.
//!
//! `Bft::step(chain, durability, event) -> effects` reads the chain only through [`Chain`] and holds no
//! clock, no key and no I/O. A driver feeds events and runs the effects in order:
//!
//! * `PersistIntent` is written and synced first; if that fails the process stops (ADR-010 section 4).
//!   No `Sign` or `Send` of the same step is run after a failed write.
//! * `Sign` asks the signer for the signature of the intent. The signer is reachable only through
//!   this effect, and the effect is made in one place ([`Bft::authorize`]), after
//!   [`decide`](super::safety::decide).
//! * `Send(Vote | Timeout)` carries the signature of the `Sign` before it. The driver also feeds the
//!   sent message back as `Event::Vote` or `Event::Timeout` with no peer, which is geth's local
//!   `voteHandler` / `timeoutHandler` call (`vote.go:269`, `timeout.go:289`).
//! * `SetTimer` replaces the round timer; a `Timer` event with an older token is ignored.
//! * `Store` asks the driver to keep this SyncInfo (the highest QC and the highest TC; chaindata, not
//!   the safety record). A QC or TC that a node formed from votes or timeouts is in no block until a
//!   leader includes it; if the node restarts and loses it while its record says it moved past that round,
//!   it cannot sign a timeout for the round the others are in (ADR-010 section 7), and when the others need
//!   that timeout the network stops. The driver keeps, for each of the two, the certificate of the higher
//!   round (a restart itself emits a lower one first). At start the driver feeds the stored SyncInfo back as
//!   `Event::SyncInfo` with no peer; it is verified like any other (found by the simulator, seeds 806 and
//!   1983).
//!
//! Where geth is stricter or looser than this machine, the page says so (`spec/consensus/v2-bft-vote-path.md`).

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use alloy_primitives::{Address, B256};
use xone_chainspec::V2;
use xone_db::Durability;
use xone_primitives::{BlockInfo, QuorumCert, SyncInfo, Timeout, TimeoutCert, Vote};

use super::{
    cert::{Scheme, Secp},
    commit::{self, Headers, Link},
    forensics::{Forensics, Proof},
    pool::Pool,
    safety::{decide, BlockRef, Intent, Kind, Refusal, SafetyRecord},
    syncinfo,
    timer::Timer,
    Error,
};

pub mod drive;
pub mod rule;
pub mod timeout;
pub mod vote;

/// A peer, as the driver names it. The machine only hands it back in `ScorePeer`.
pub type Peer = u64;

/// geth `voteRoundLookahead` (`vote.go:132`): rounds past the current one whose votes are pooled.
pub const VOTE_LOOKAHEAD: u64 = 5;
/// geth `PoolHygieneRound` (`engine.go:44`): rounds behind the current one that a pool keeps.
pub const POOL_HYGIENE_ROUND: u64 = 10;

/// What the machine needs of a header (geth `getExtraFields`, `engine.go:2380`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderFacts {
    pub number: u64,
    pub hash: B256,
    pub parent: B256,
    /// 0 for the V2 switch block.
    pub round: u64,
    /// The QC the header carries. `None` for the V2 switch block.
    pub qc: Option<QuorumCert>,
}

/// geth `EpochSwitchInfo` of the epoch that holds a block or a round (`getEpochSwitchInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochFacts {
    /// The epoch's masternodes (not the candidate pool).
    pub masternodes: Vec<Address>,
    /// The gap number the epoch's messages carry.
    pub gap: u64,
    /// The hash of the epoch switch block.
    pub switch: B256,
}

/// Read-only chain facts. An answer that cannot be derived is an `Err`, never a guess: a driver
/// derives the epoch of a block from its parent walk, not from a table written on import, so that a
/// validator that joined mid-V2 or crossed the switch through sync has the same facts as one that
/// imported every block (xOneGo #718).
pub trait Chain {
    /// A header the node has executed, canonical or on a side branch.
    fn header(&self, hash: &B256) -> Option<Arc<HeaderFacts>>;
    /// The chain head (geth `CurrentHeader`).
    fn head(&self) -> Option<Arc<HeaderFacts>>;
    /// The epoch that holds block `hash`.
    fn epoch(&self, hash: &B256) -> Result<Arc<EpochFacts>, Error>;
    /// The epoch that holds `round` (geth `getTCEpochInfo`, `timeout.go:334`).
    fn epoch_of_round(&self, round: u64) -> Result<Arc<EpochFacts>, Error>;
    /// The candidate pool of the gap snapshot at `gap`: who may sign a vote or a timeout.
    fn candidates(&self, gap: u64) -> Option<Arc<[Address]>>;
    /// The gap number a timeout of `round` carries (geth `sendTimeout`, `timeout.go:226-264`).
    fn timeout_gap(&self, round: u64) -> Result<u64, Error>;
}

#[derive(Debug, Clone)]
pub enum Event {
    /// The chain head moved, for example by sync: seed the context if it is not seeded.
    Synced,
    /// A block that the node has executed, as a proposal (geth `ProposedBlockHandler`).
    Proposal(B256),
    /// A vote. `None` is the local node.
    Vote(Vote, Option<Peer>),
    Timeout(Timeout, Option<Peer>),
    SyncInfo(SyncInfo, Option<Peer>),
    /// The round timer with this token expired.
    Timer(u64),
}

/// Messages without the signature: the driver adds the signature of the `Sign` before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    Vote { info: BlockInfo, gap: u64 },
    Timeout { round: u64, gap: u64 },
    SyncInfo(SyncInfo),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offence {
    /// The message fails a validity rule (bad signature, not a member, below the threshold).
    Invalid,
    /// More signatures than any certificate can hold (geth `maxPeerSigCap`).
    Oversized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    PersistIntent {
        intent: Intent,
        record: SafetyRecord,
    },
    Sign(Intent),
    Send(Outbound),
    SetTimer {
        token: u64,
        after: Duration,
    },
    /// The machine entered `round`. `mine_period` is the `minePeriod` of the V2 config of that round
    /// (geth `minePeriodCh`): a producer waits this many seconds after the parent before it seals.
    AdvanceRound {
        round: u64,
        mine_period: u64,
    },
    Commit(Link),
    /// The highest QC or the highest TC was raised: keep this SyncInfo across a restart.
    Store(SyncInfo),
    ScorePeer {
        peer: Peer,
        offence: Offence,
    },
    Forensics(Proof),
    /// The safety unit refused to sign.
    Refused {
        kind: Kind,
        why: Refusal,
    },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub v2: V2,
    /// `XDPoS.gap`.
    pub gap: u64,
    /// The address of the validator key, `None` for a follower: it never signs.
    pub me: Option<Address>,
}

/// One validator's BFT state. Fields are visible to the sibling rule modules of `v2`.
pub struct Bft<S: Scheme = Secp> {
    pub(super) cfg: Config,
    pub(super) scheme: S,
    pub(super) dur: Durability,
    /// The signing record. It is replaced when a `PersistIntent` is emitted, not when it is acked: a
    /// failed write stops the process (ADR-010 section 6), so a later step never reads a record the
    /// disk does not hold.
    pub(super) rec: SafetyRecord,
    pub(super) init: bool,
    pub(super) round: u64,
    pub(super) timeout_count: u64,
    pub(super) highest_qc: QuorumCert,
    pub(super) highest_tc: TimeoutCert,
    pub(super) committed: Option<Link>,
    pub(super) votes: Pool<Vote>,
    pub(super) timeouts: Pool<Timeout>,
    pub(super) timer: Timer,
    /// Proposals for a later round that arrived before its TC, by round (one each).
    pub(super) stash: BTreeMap<u64, B256>,
    pub(super) forensics: Forensics,
}

struct Links<'a, C>(&'a C);

impl<C: Chain> Headers for Links<'_, C> {
    fn link(&self, hash: B256) -> Option<Link> {
        self.0.header(&hash).map(|h| Link {
            number: h.number,
            round: h.round,
            hash: h.hash,
            parent: h.parent,
        })
    }
}

pub(super) fn block_ref(i: &BlockInfo) -> BlockRef {
    BlockRef {
        hash: i.hash,
        round: i.round,
        number: i.number,
    }
}

/// Score the sender of a message that failed a validity rule. A failure that is the node's own lack
/// of data (a header or epoch it does not have) is not the peer's fault.
pub(super) fn score(from: Option<Peer>, e: &Error, fx: &mut Vec<Effect>) {
    let offence = match e {
        Error::TooManySignatures { .. } => Offence::Oversized,
        Error::SignatureUnrecoverable
        | Error::SignatureHighS
        | Error::SignerNotMasternode(_)
        | Error::BelowThreshold { .. }
        | Error::BlockInfoMismatch
        | Error::GapMismatch { .. }
        | Error::Rlp(_) => Offence::Invalid,
        _ => return,
    };
    if let Some(peer) = from {
        fx.push(Effect::ScorePeer { peer, offence });
    }
}

impl<S: Scheme> Bft<S> {
    /// `rec` is the record the safety store loaded (geth `loadBFTSafetyState`, `engine.go:581`).
    pub fn new(cfg: Config, scheme: S, rec: SafetyRecord) -> Result<Self, Error> {
        let row0 = cfg.v2.config_at_round(0).ok_or(Error::NoV2Config(0))?;
        let timer = Timer::new(row0);
        Ok(Self {
            cfg,
            scheme,
            dur: Durability::Durable,
            rec,
            init: false,
            round: 0,
            timeout_count: 0,
            // geth `createEngine`, `engine.go:196-207`
            highest_qc: QuorumCert {
                proposed_block_info: BlockInfo {
                    hash: B256::ZERO,
                    round: 0,
                    number: 0,
                },
                signatures: vec![],
                gap_number: 0,
            },
            highest_tc: TimeoutCert {
                round: 0,
                signatures: vec![],
                gap_number: 0,
            },
            committed: None,
            votes: Pool::default(),
            timeouts: Pool::default(),
            timer,
            stash: BTreeMap::new(),
            forensics: Forensics::default(),
        })
    }

    pub fn round(&self) -> u64 {
        self.round
    }
    pub fn record(&self) -> &SafetyRecord {
        &self.rec
    }
    pub fn highest_qc(&self) -> &QuorumCert {
        &self.highest_qc
    }
    pub fn highest_tc(&self) -> &TimeoutCert {
        &self.highest_tc
    }
    pub fn committed(&self) -> Option<&Link> {
        self.committed.as_ref()
    }
    pub fn initialized(&self) -> bool {
        self.init
    }
    /// The votes the machine holds for `round`, by signer (for a node's diagnostics).
    pub fn votes_at(&self, round: u64) -> impl Iterator<Item = (&Address, &Vote)> {
        self.votes.entries(round)
    }

    /// Messages held in the vote pool and the timeout pool.
    pub fn pooled(&self) -> (usize, usize) {
        (self.votes.len(), self.timeouts.len())
    }

    /// One event. Effects are in the order the driver must run them.
    pub fn step<C: Chain>(&mut self, c: &C, dur: Durability, ev: Event) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.dur = dur;
        if !self.init && !self.try_init(c, &mut fx) {
            return fx;
        }
        match ev {
            Event::Synced => {}
            Event::Proposal(h) => vote::on_proposal(self, c, h, &mut fx),
            Event::Vote(v, from) => vote::on_vote(self, c, v, from, &mut fx),
            Event::Timeout(t, from) => timeout::on_timeout(self, c, t, from, &mut fx),
            Event::SyncInfo(s, from) => syncinfo::on_sync_info(self, c, s, from, &mut fx),
            Event::Timer(token) => timeout::on_timer(self, c, token, &mut fx),
        }
        // a proposal that came before the TC of its round is voted when the round starts
        let r = self.round;
        self.stash.retain(|k, _| *k >= r);
        if let Some(h) = self.stash.remove(&r) {
            vote::on_proposal(self, c, h, &mut fx);
        }
        fx
    }

    /// geth `initial` (`engine.go:370-570`): at the V2 switch block the highest QC is the round-0 QC
    /// of that block and the round is 1; above it the highest QC is the one in the head and the round
    /// follows from it. A head below the switch leaves the machine unseeded, and every event but
    /// `Synced` is dropped until the chain has crossed it.
    fn try_init<C: Chain>(&mut self, c: &C, fx: &mut Vec<Effect>) -> bool {
        let Some(head) = c.head() else {
            return false;
        };
        let switch = self.cfg.v2.switch_block;
        if head.number < switch {
            return false;
        }
        if head.number == switch {
            let gap = head.number.saturating_sub(self.cfg.gap);
            self.highest_qc = QuorumCert {
                proposed_block_info: BlockInfo {
                    hash: head.hash,
                    round: 0,
                    number: head.number,
                },
                signatures: vec![],
                gap_number: gap,
            };
            self.round = 1;
        } else {
            let Some(qc) = head.qc.clone() else {
                return false;
            };
            // the QC's block may be missing during a sync: its processing is deferred
            if c.header(&qc.proposed_block_info.hash).is_some() {
                // its round and timer are replaced below, as geth's final `Reset(0, 0)` replaces them
                let mut seeded = Vec::new();
                if self.process_qc(c, &qc, &mut seeded).is_err() {
                    return false;
                }
                fx.extend(seeded.into_iter().filter(|e| {
                    !matches!(e, Effect::AdvanceRound { .. } | Effect::SetTimer { .. })
                }));
            }
            self.round = self.round.max(1);
        }
        let Some(row) = self.cfg.v2.config_at_round(self.round) else {
            return false;
        };
        let mine_period = row.mine_period;
        let (token, after) = self.timer.reset(0, 0);
        fx.push(Effect::AdvanceRound {
            round: self.round,
            mine_period,
        });
        fx.push(Effect::SetTimer { token, after });
        self.init = true;
        true
    }

    /// geth `setNewRound` (`engine.go:3220-3250`).
    pub(super) fn set_new_round(&mut self, round: u64, fx: &mut Vec<Effect>) -> Result<(), Error> {
        let row = self
            .cfg
            .v2
            .config_at_round(round)
            .ok_or(Error::NoV2Config(round))?;
        self.round = round;
        self.timeout_count = 0;
        self.timer.set_params(row);
        let mine_period = row.mine_period;
        let (token, after) = self
            .timer
            .reset(round, self.highest_qc.proposed_block_info.round);
        self.timeouts.clear();
        self.votes
            .prune_below(round.saturating_sub(POOL_HYGIENE_ROUND));
        fx.push(Effect::AdvanceRound { round, mine_period });
        fx.push(Effect::SetTimer { token, after });
        Ok(())
    }

    /// geth `processQC` (`utils.go:476-528`): highest QC, lock QC, the 3-chain commit, the next round.
    /// The highest QC is raised before the header is looked up, as geth does.
    pub(super) fn process_qc<C: Chain>(
        &mut self,
        c: &C,
        qc: &QuorumCert,
        fx: &mut Vec<Effect>,
    ) -> Result<(), Error> {
        let info = &qc.proposed_block_info;
        // A machine starts with a placeholder QC (no block). geth raises a QC only by round, so a head
        // whose QC is the round-0 QC of the switch block leaves the placeholder, and a SyncInfo made from
        // it names no block and is refused by every peer; here the first QC replaces it.
        let unset = self.highest_qc.proposed_block_info.hash.is_zero();
        if unset || info.round > self.highest_qc.proposed_block_info.round {
            self.highest_qc = qc.clone();
            self.rec.observe_qc(block_ref(info), None);
            fx.push(Effect::Store(syncinfo::originate(self)));
        }
        let header = c
            .header(&info.hash)
            .ok_or(Error::MissingHeader { hash: info.hash })?;
        let switch = self.cfg.v2.switch_block;
        if header.number > switch {
            let inner = header.qc.as_ref().ok_or(Error::Missing {
                what: "QC of the certified block",
                number: header.number,
            })?;
            self.rec
                .observe_qc(block_ref(info), Some(block_ref(&inner.proposed_block_info)));
            let proposed = Link {
                number: header.number,
                round: header.round,
                hash: header.hash,
                parent: header.parent,
            };
            if let Some(grand) =
                commit::commit_blocks(&Links(c), switch, self.committed.as_ref(), &proposed)?
            {
                self.committed = Some(grand);
                fx.push(Effect::Commit(grand));
                if let Some(parent) = c.header(&header.parent) {
                    if let Some(p) = self
                        .forensics
                        .monitor(c, &self.scheme, &parent, &header, qc)
                    {
                        fx.push(Effect::Forensics(p));
                    }
                }
            }
        }
        if info.round >= self.round {
            self.set_new_round(info.round.saturating_add(1), fx)?;
        }
        Ok(())
    }

    /// geth `processTC` (`timeout.go:474-486`).
    pub(super) fn process_tc(
        &mut self,
        tc: &TimeoutCert,
        fx: &mut Vec<Effect>,
    ) -> Result<(), Error> {
        if tc.round > self.highest_tc.round {
            self.highest_tc = tc.clone();
            self.rec.observe_tc(tc.round);
            fx.push(Effect::Store(syncinfo::originate(self)));
        }
        if tc.round >= self.round {
            self.set_new_round(tc.round.saturating_add(1), fx)?;
        }
        Ok(())
    }

    /// The only place that makes `Sign` and `Send`: the safety unit decides, the record is persisted
    /// first (unless the intent is a retry that the record already holds), then the signature, then
    /// the message.
    pub(super) fn authorize(
        &mut self,
        intent: Intent,
        out: Outbound,
        fx: &mut Vec<Effect>,
    ) -> bool {
        match decide(&self.rec, &intent, self.dur) {
            Ok(next) => {
                if next != self.rec {
                    fx.push(Effect::PersistIntent {
                        intent,
                        record: next.clone(),
                    });
                    self.rec = next;
                }
                fx.push(Effect::Sign(intent));
                fx.push(Effect::Send(out));
                true
            }
            Err(why) => {
                fx.push(Effect::Refused {
                    kind: intent.kind,
                    why,
                });
                false
            }
        }
    }

    /// geth `VerifyBlockInfo` (`utils.go:535-575`): the header exists and agrees with the info.
    pub(super) fn verify_block_info<C: Chain>(
        &self,
        c: &C,
        info: &BlockInfo,
    ) -> Result<Arc<HeaderFacts>, Error> {
        let h = c
            .header(&info.hash)
            .ok_or(Error::MissingHeader { hash: info.hash })?;
        let round_ok = if info.number == self.cfg.v2.switch_block {
            info.round == 0
        } else {
            h.round == info.round
        };
        if h.number != info.number || !round_ok {
            return Err(Error::BlockInfoMismatch);
        }
        Ok(h)
    }
}
