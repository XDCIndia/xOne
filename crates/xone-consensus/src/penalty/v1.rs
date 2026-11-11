//! V1 penalties from `TIPSigning` on (geth `HookPenaltyTIPSigning`, `eth/hooks/hooks.go:182-302`).
//!
//! At a checkpoint the masternodes of the previous epoch that sealed no block of this one are
//! penalised. Nodes penalised `LimitPenaltyEpoch + 1` epochs ago that are candidates again stay
//! penalised unless they signed in the last 150 blocks; from `TIPRandomize` the penalty list is
//! both, before it only the comeback nodes. The pre-`TIPSigning` rule reads the 0x89 contract
//! state and is not implemented.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256};
use xone_chainspec::ChainSpec;

use super::comeback::{scan, Release};
use crate::chain::{header_at, BlockReader, Error, RANGE_RETURN_SIGNER};

/// geth `common.LimitPenaltyEpoch`, `common/constants.go:12`.
pub const LIMIT_PENALTY_EPOCH: u64 = 4;

/// The checkpoint being sealed or verified.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub number: u64,
    /// Hash of block `number - 1`.
    pub parent: B256,
    pub candidates: &'a [Address],
    /// The masternodes of the header at `number - epoch`.
    pub prev_masternodes: &'a [Address],
    /// The `Penalties` of the header at [`comeback_epoch`], when there is one.
    pub comeback_penalties: Option<&'a [Address]>,
}

/// The block whose `Penalties` feed the comeback: `number - (LimitPenaltyEpoch + 1) * epoch`, none
/// while the chain is that short (`hooks.go:199-203`).
pub fn comeback_epoch(spec: &ChainSpec, number: u64) -> Option<u64> {
    let length = (LIMIT_PENALTY_EPOCH + 1) * spec.xdpos.epoch;
    (number > length).then(|| number - length)
}

pub fn penalties<R: BlockReader>(
    spec: &ChainSpec,
    reader: &R,
    input: &Input<'_>,
) -> Result<Vec<Address>, Error> {
    let (number, epoch) = (input.number, spec.xdpos.epoch);
    if number < epoch {
        return Ok(Vec::new());
    }
    // Blocks number-1 down to number-epoch, newest first; the sealers of all but the last.
    let mut blocks = Vec::new();
    let mut mined: BTreeMap<Address, u64> = BTreeMap::new();
    let (mut hash, mut at) = (input.parent, number - 1);
    for _ in 1..epoch {
        let h = header_at(reader, hash, at)?;
        blocks.push((at, hash));
        *mined.entry(h.miner).or_default() += 1;
        hash = h.parent_hash;
        at -= 1;
    }
    blocks.push((at, hash));
    blocks.reverse();

    let mut out: Vec<Address> = mined
        .iter()
        .filter(|(_, &n)| n < 1)
        .map(|(&a, _)| a)
        .collect();
    out.extend(
        input
            .prev_masternodes
            .iter()
            .filter(|a| !mined.contains_key(*a)),
    );

    let mut release = Release {
        pending: Vec::new(),
    };
    if let Some(at) = comeback_epoch(spec, number) {
        let list = input.comeback_penalties.ok_or(Error::Missing {
            what: "comeback epoch header",
            number: at,
            hash: B256::ZERO,
        })?;
        release.pending = list
            .iter()
            .flat_map(|p| input.candidates.iter().filter(move |c| *c == p))
            .copied()
            .collect();
    }
    let take = RANGE_RETURN_SIGNER.min(blocks.len());
    scan(reader, &blocks[blocks.len() - take..], &mut release)?;
    if spec.effective.tip_randomize.is_active(number) {
        out.extend(&release.pending);
        return Ok(out);
    }
    Ok(release.pending)
}
