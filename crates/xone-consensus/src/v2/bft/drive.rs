//! Running the effects of one step of the machine (the driver's half of `spec/consensus/v2-bft-vote-path.md`):
//! the one place where `PersistIntent`, `Sign` and `Send` meet the safety unit. The unit writes the record and
//! signs in one call, in that order ([`Sealer::sign_joined`]); a `Send` carries the signature of the `Sign`
//! before it. Every other effect is handed back to the caller to send, store or schedule.

use std::time::Duration;

use xone_primitives::{SyncInfo, Timeout, Vote};

use super::{Bft, Effect, Offence, Outbound, Peer};
use crate::{
    unit::{Sealer, UnitError},
    v2::{
        cert::Scheme,
        commit::Link,
        forensics::Proof,
        safety::{Intent, Kind, Refusal},
    },
};

/// What a step asks of the world, signed.
#[derive(Debug, Default)]
pub struct Driven {
    pub votes: Vec<Vote>,
    pub timeouts: Vec<Timeout>,
    pub sync_infos: Vec<SyncInfo>,
    /// `(round, minePeriod)` of every round entered.
    pub rounds: Vec<(u64, u64)>,
    pub commits: Vec<Link>,
    /// The last timer to set, if any.
    pub timer: Option<(u64, Duration)>,
    pub stores: Vec<SyncInfo>,
    pub scores: Vec<(Peer, Offence)>,
    pub proofs: Vec<Proof>,
    pub refused: Vec<(Kind, Refusal)>,
}

/// Runs `fx`, the effects of `bft.step`, against the unit. An error is a failed write or a failed signature:
/// the caller stops (ADR-010 section 6), and nothing after the failure was sent.
pub fn drive<S: Scheme, U: Sealer>(
    bft: &Bft<S>,
    unit: &mut U,
    fx: Vec<Effect>,
) -> Result<Driven, UnitError> {
    let mut out = Driven::default();
    let mut signed: Option<(Intent, xone_primitives::Signature)> = None;
    for e in fx {
        match e {
            // the unit joins this record into its own and writes it before the signature exists
            Effect::PersistIntent { .. } => {}
            Effect::Sign(i) => signed = Some((i, unit.sign_joined(i, bft.record())?)),
            Effect::Send(o) => match (o, signed.take()) {
                (Outbound::Vote { info, gap }, Some((_, s))) => out.votes.push(Vote {
                    proposed_block_info: info,
                    signature: s.bytes,
                    gap_number: gap,
                }),
                (Outbound::Timeout { round, gap }, Some((_, s))) => out.timeouts.push(Timeout {
                    round,
                    signature: s.bytes,
                    gap_number: gap,
                }),
                (Outbound::SyncInfo(s), _) => out.sync_infos.push(s),
                _ => return Err(UnitError::Sign("a message without its signature".into())),
            },
            Effect::SetTimer { token, after } => out.timer = Some((token, after)),
            Effect::AdvanceRound { round, mine_period } => out.rounds.push((round, mine_period)),
            Effect::Commit(l) => out.commits.push(l),
            Effect::Store(s) => out.stores.push(s),
            Effect::ScorePeer { peer, offence } => out.scores.push((peer, offence)),
            Effect::Forensics(p) => out.proofs.push(p),
            Effect::Refused { kind, why } => out.refused.push((kind, why)),
        }
    }
    Ok(out)
}
