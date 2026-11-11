//! Quorum and timeout certificates: which signatures count and how many are enough.
//!
//! geth `recoverUniqueSigners` (`engine_v2/utils.go:131-250`; `hasLowS` `:86`, the member check `:210`, byte-duplicate skip `:221`): every signature must recover, be in
//! low-S form and come from a masternode of the epoch, or the whole certificate is refused. Two
//! signatures that recover the same address count once (F-2026-19525: one key can make any number
//! of distinct valid signatures). The threshold gate is a float comparison,
//! `float64(unique) < float64(len(masternodes)) * certThreshold` (`utils.go:373`, `timeout.go:436`),
//! of the config at the round of the certificate.

use std::collections::BTreeSet;

use alloy_primitives::{keccak256, Address, Bytes, B256};
use alloy_rlp::RlpEncodable;
use xone_primitives::{ecrecover, BlockInfo, QuorumCert, TimeoutCert};

use super::Error;

/// geth `types.VoteForSign`: what a QC signature signs (`core/types/consensus_v2.go:228`).
#[derive(RlpEncodable)]
struct VoteForSign<'a> {
    proposed_block_info: &'a BlockInfo,
    gap_number: u64,
}

/// geth `types.TimeoutForSign` (`core/types/consensus_v2.go:239`).
#[derive(RlpEncodable)]
struct TimeoutForSign {
    round: u64,
    gap_number: u64,
}

pub fn qc_digest(qc: &QuorumCert) -> B256 {
    keccak256(alloy_rlp::encode(VoteForSign {
        proposed_block_info: &qc.proposed_block_info,
        gap_number: qc.gap_number,
    }))
}

pub fn tc_digest(tc: &TimeoutCert) -> B256 {
    keccak256(alloy_rlp::encode(TimeoutForSign {
        round: tc.round,
        gap_number: tc.gap_number,
    }))
}

/// geth `recoverUniqueSigners` with the allowed set: the distinct masternodes that signed `digest`.
/// Byte-identical signatures recover the same address, so the repeat is skipped (`utils.go:221-227`).
pub fn unique_signers(
    digest: &B256,
    signatures: &[Bytes],
    masternodes: &[Address],
) -> Result<BTreeSet<Address>, Error> {
    let mut seen = BTreeSet::new();
    let mut signers = BTreeSet::new();
    for sig in signatures {
        if !seen.insert(sig) {
            continue;
        }
        let who = ecrecover(digest, sig).map_err(|_| Error::SignatureUnrecoverable)?;
        if !low_s(sig) {
            return Err(Error::SignatureHighS);
        }
        if !masternodes.contains(&who) {
            return Err(Error::SignerNotMasternode(who));
        }
        signers.insert(who);
    }
    Ok(signers)
}

/// geth `hasLowS` (`utils.go:86-101`): `s` is at most half the curve order.
fn low_s(sig: &[u8]) -> bool {
    use k256::ecdsa::Signature;
    sig.get(..64)
        .and_then(|rs| Signature::from_slice(rs).ok())
        .is_some_and(|s| s.normalize_s().is_none())
}

/// The smallest signer count that passes `float64(count) < float64(n) * threshold`.
pub fn min_signers(masternodes: usize, threshold: f64) -> Result<usize, Error> {
    let n = u32::try_from(masternodes).map_err(|_| Error::TooMany(masternodes))?;
    let need = f64::from(n) * threshold;
    let mut k: u32 = 0;
    // past `n` no count can pass, so the search stops there
    while k <= n && f64::from(k) < need {
        k += 1;
    }
    usize::try_from(k).map_err(|_| Error::TooMany(masternodes))
}

fn check(unique: usize, masternodes: usize, threshold: f64) -> Result<(), Error> {
    let required = min_signers(masternodes, threshold)?;
    if unique < required {
        return Err(Error::BelowThreshold {
            unique,
            required,
            masternodes,
        });
    }
    Ok(())
}

/// geth `verifyQC` signature part (`utils.go:315`, threshold gate `:373`). `masternodes` are those of the epoch of the
/// QC's block, `threshold` the `certificateThreshold` of the config at the QC's round. A QC of round
/// 0 (the V2 switch block) passes the threshold with no signature, but every signature it carries
/// must still be valid. Gap number and block info are checked by the caller (header verifier).
pub fn verify_qc(qc: &QuorumCert, masternodes: &[Address], threshold: f64) -> Result<(), Error> {
    if masternodes.is_empty() {
        return Err(Error::NoMasternodes);
    }
    let signers = unique_signers(&qc_digest(qc), &qc.signatures, masternodes)?;
    if qc.proposed_block_info.round > 0 {
        check(signers.len(), masternodes.len(), threshold)?;
    }
    Ok(())
}

/// What `verifyTC` reads besides the certificate.
#[derive(Debug, Clone, Copy)]
pub struct TcContext<'a> {
    /// Candidates of the gap snapshot at `tc.gap_number`: only a sanity check, but an empty snapshot
    /// refuses the TC (`timeout.go:383-394`).
    pub snapshot_candidates: usize,
    /// Masternodes of the epoch that holds the TC's round (geth `getTCEpochInfo`, `timeout.go:335`).
    pub masternodes: &'a [Address],
    /// `certificateThreshold` of the config at the TC's round.
    pub threshold: f64,
}

/// geth `verifyTC` (`timeout.go:373-465`, gate `:436`). There is no round-0 exemption: a TC with no signatures
/// fails the threshold.
pub fn verify_tc(tc: &TimeoutCert, ctx: &TcContext<'_>) -> Result<(), Error> {
    if ctx.snapshot_candidates == 0 {
        return Err(Error::SnapshotEmpty);
    }
    if ctx.masternodes.is_empty() {
        return Err(Error::NoMasternodes);
    }
    let signers = unique_signers(&tc_digest(tc), &tc.signatures, ctx.masternodes)?;
    check(signers.len(), ctx.masternodes.len(), ctx.threshold)
}

/// geth `isBlankTC` (`utils.go:739`): the bootstrap TC a node holds before any timeout, round 0 and
/// no signature. The gap number is not looked at.
pub fn is_blank_tc(tc: &TimeoutCert) -> bool {
    tc.round == 0 && tc.signatures.is_empty()
}

/// The TC of a SyncInfo, `VerifySyncInfoMessage` (`utils.go:672-733`, blank TC `:714`): the blank bootstrap TC passes
/// unverified, every other TC goes through [`verify_tc`]. This is the only place geth accepts it.
pub fn verify_sync_info_tc(tc: &TimeoutCert, ctx: &TcContext<'_>) -> Result<(), Error> {
    if is_blank_tc(tc) {
        return Ok(());
    }
    verify_tc(tc, ctx)
}

/// geth `rlp.DecodeBytes` into a TC: exactly one value, canonical integers, no extra element.
pub fn decode_tc(bytes: &[u8]) -> Result<TimeoutCert, Error> {
    Ok(alloy_rlp::decode_exact(bytes)?)
}

pub fn decode_qc(bytes: &[u8]) -> Result<QuorumCert, Error> {
    Ok(alloy_rlp::decode_exact(bytes)?)
}
