//! SyncInfo: what a node sends so that a peer that is behind catches up, and what it does with one
//! it receives.
//!
//! Origination: after a TC (`timeout.go:214-216`) and every `TimeoutSyncThreshold` timeouts of a round
//! (`timeout.go:321-326`).
//!
//! Handling (geth `VerifySyncInfoMessage`, `utils.go:672-733`, then `SyncInfoHandler`, `:744`): a message
//! that is not newer than the local highest QC and TC is dropped; one with more signatures than
//! `maxPeerSigCap` is refused before any signature work; the QC and the TC are verified in full. Only
//! after that is any state changed. A forged or under-threshold certificate changes nothing and the
//! sender is scored.

use xone_primitives::{BlockInfo, QuorumCert, SyncInfo, TimeoutCert};

use crate::v2::{
    bft::{score, Bft, Chain, Effect, Peer},
    cert::{is_blank_tc, verify_qc_with, verify_sync_info_tc_with, Scheme, TcContext},
    Error,
};

/// geth `maxPeerSigCap` (`utils.go:28`).
pub const MAX_PEER_SIGNATURES: usize = 150;

/// geth `getSyncInfo` (`engine.go:3212`).
pub fn originate<S: Scheme>(m: &Bft<S>) -> SyncInfo {
    SyncInfo {
        highest_quorum_cert: m.highest_qc.clone(),
        highest_timeout_cert: m.highest_tc.clone(),
    }
}

pub(crate) fn on_sync_info<S: Scheme, C: Chain>(
    m: &mut Bft<S>,
    c: &C,
    info: SyncInfo,
    from: Option<Peer>,
    fx: &mut Vec<Effect>,
) {
    let (qc, tc) = (&info.highest_quorum_cert, &info.highest_timeout_cert);
    if m.highest_qc.proposed_block_info.round >= qc.proposed_block_info.round
        && m.highest_tc.round >= tc.round
    {
        return; // not newer
    }
    if let Err(e) = verify(m, c, qc, tc) {
        return score(from, &e, fx);
    }
    // verified: now the state may change
    if m.process_qc(c, qc, fx).is_ok() {
        let _ = m.process_tc(tc, fx);
    }
}

fn verify<S: Scheme, C: Chain>(
    m: &Bft<S>,
    c: &C,
    qc: &QuorumCert,
    tc: &TimeoutCert,
) -> Result<(), Error> {
    let too_many = |n: usize| {
        (n > MAX_PEER_SIGNATURES).then_some(Error::TooManySignatures {
            n,
            cap: MAX_PEER_SIGNATURES,
        })
    };
    if let Some(e) = too_many(qc.signatures.len()) {
        return Err(e);
    }
    verify_qc(m, c, qc)?;
    if is_blank_tc(tc) {
        return Ok(()); // the sender has seen no timeout yet (`utils.go:714`)
    }
    if let Some(e) = too_many(tc.signatures.len()) {
        return Err(e);
    }
    let row = m
        .cfg
        .v2
        .config_at_round(tc.round)
        .ok_or(Error::NoV2Config(tc.round))?;
    let epoch = c.epoch_of_round(tc.round)?;
    let ctx = TcContext {
        snapshot_candidates: c.candidates(tc.gap_number).map_or(0, |l| l.len()),
        masternodes: &epoch.masternodes,
        threshold: row.cert_threshold,
    };
    verify_sync_info_tc_with(&m.scheme, tc, &ctx)
}

/// geth `verifyQC` (`utils.go:315-460`): the signatures against the masternodes of the epoch of the
/// QC's block, the threshold of the config at the QC's round, the gap number of that epoch, and the
/// block. The check of the gap against a matching snapshot instead (`utils.go:373-396`, a bulk-sync
/// device of the fork) is not made: a mismatch is refused.
fn verify_qc<S: Scheme, C: Chain>(m: &Bft<S>, c: &C, qc: &QuorumCert) -> Result<(), Error> {
    let info: &BlockInfo = &qc.proposed_block_info;
    let epoch = c.epoch(&info.hash)?;
    let row = m
        .cfg
        .v2
        .config_at_round(info.round)
        .ok_or(Error::NoV2Config(info.round))?;
    verify_qc_with(&m.scheme, qc, &epoch.masternodes, row.cert_threshold)?;
    if epoch.gap != qc.gap_number {
        return Err(Error::GapMismatch {
            expected: epoch.gap,
            got: qc.gap_number,
        });
    }
    m.verify_block_info(c, info).map(|_| ())
}
