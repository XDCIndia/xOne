//! V2 penalties at an epoch switch (geth `HookPenalty`, `eth/hooks/engine_v2_hooks.go:318-565`,
//! called from `calcMasternodes`, `engine_v2/engine.go:2019-2095`).
//!
//! A masternode is penalised when it mined fewer than the minimum blocks of the epoch that ends,
//! or none. Before `TIPUpgradePenalty` the minimum is 1 and a node penalised at the previous switch
//! that is a candidate again and signed in the last 150 blocks is released. From it the minimum is
//! the config's `MinimumMinerBlockPerEpoch`, and a node stays penalised until it has been listed
//! for `LimitPenaltyEpoch` epochs in a row and has `MinimumSigningTx` qualifying sign
//! transactions in the epoch. At the V2 switch block plus one nobody is penalised (`engine.go:2054`).

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256};
use xone_chainspec::ChainSpec;

use super::comeback::{scan, Release, Tally};
use super::same_set;
use crate::chain::{header_at, BlockReader, Error, PenaltyHistory, RANGE_RETURN_SIGNER};
use crate::reward::tiered::config;

/// geth `common.LimitPenaltyEpochV2`, `common/constants.go:13`: before the upgrade the comeback
/// looks at the previous switch only.
const LIMIT_PENALTY_EPOCH_V2: u64 = 0;
/// geth `common.MinimunMinerBlockPerEpoch`, `common/constants.go:17`.
const MINIMUM_MINER_BLOCK_PER_EPOCH: u64 = 1;

/// The epoch switch being sealed or verified.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub number: u64,
    /// Hash of block `number - 1`.
    pub parent: B256,
    /// Round of the parent header; it selects the V2 config.
    pub round: u64,
    /// `GetMasternodesByHash(parent)`: the masternodes of the epoch that ends.
    pub parent_masternodes: &'a [Address],
    /// The candidates of the snapshot (`NextEpochCandidates`).
    pub candidates: &'a [Address],
}

/// geth `resolvePostUpgradeLimitPenaltyEpoch` (`engine_v2_hooks.go:44`): the config's value, or 1.
pub fn post_upgrade_limit(limit_penalty_epoch: u64) -> u64 {
    match limit_penalty_epoch {
        0 => 1,
        n => n,
    }
}

fn history<H: PenaltyHistory>(
    h: &H,
    parent: B256,
    number: u64,
    back: usize,
) -> Result<Vec<Address>, Error> {
    h.penalties(parent, back).ok_or(Error::Missing {
        what: "epoch-switch penalties",
        number: number - 1,
        hash: parent,
    })
}

/// The penalty list of the epoch switch `input.number`, in a fixed order (geth's order follows a
/// map and differs between runs; verification compares as sets, [`super::same_set`]).
pub fn penalties<R: BlockReader + PenaltyHistory>(
    spec: &ChainSpec,
    reader: &R,
    input: &Input<'_>,
) -> Result<Vec<Address>, Error> {
    let v2 = spec.xdpos.v2.as_ref().ok_or(Error::NoV2)?;
    let number = input.number;
    if number <= v2.switch_block {
        return Err(Error::NotV2(number));
    }
    if number == v2.switch_block + 1 {
        return Ok(Vec::new());
    }
    let cfg = config(spec, input.round)?;

    // The blocks of the epoch that ends, the epoch switch block itself first (it is listed, not
    // counted), and the blocks each masternode mined.
    let mut blocks = Vec::new();
    let mut mined: BTreeMap<Address, u64> = BTreeMap::new();
    let (mut hash, mut at) = (input.parent, number - 1);
    loop {
        let h = header_at(reader, hash, at)?;
        blocks.push((at, hash));
        if h.epoch_switch {
            break;
        }
        if at == 0 {
            return Err(Error::WindowNotFound(number));
        }
        *mined.entry(h.miner).or_default() += 1;
        hash = h.parent_hash;
        at -= 1;
    }
    blocks.reverse();

    let upgraded = spec.tip_upgrade_penalty.is_active(number);
    let minimum = if upgraded {
        cfg.minimum_miner_block_per_epoch
    } else {
        MINIMUM_MINER_BLOCK_PER_EPOCH
    };
    let mut out: Vec<Address> = mined
        .iter()
        .filter(|(_, &n)| n < minimum)
        .map(|(&a, _)| a)
        .collect();
    out.extend(
        input
            .parent_masternodes
            .iter()
            .filter(|a| !mined.contains_key(*a)),
    );

    let epoch = spec.xdpos.epoch;
    let last = |n: usize| &blocks[blocks.len() - n.min(blocks.len())..];
    if !upgraded {
        if number > (LIMIT_PENALTY_EPOCH_V2 + 1) * epoch + v2.switch_block {
            let mut release = Release {
                pending: history(reader, input.parent, number, 0)?
                    .into_iter()
                    .filter(|p| input.candidates.contains(p))
                    .collect(),
            };
            scan(reader, last(RANGE_RETURN_SIGNER), &mut release)?;
            for a in release.pending {
                if !out.contains(&a) {
                    out.push(a);
                }
            }
        }
    } else {
        let limit = post_upgrade_limit(cfg.limit_penalty_epoch);
        if number > limit * epoch + v2.switch_block {
            let mut parole: BTreeMap<Address, u64> = BTreeMap::new();
            let mut newest = Vec::new();
            for back in 0..usize::try_from(limit).map_err(|_| Error::Amount)? {
                let list = history(reader, input.parent, number, back)?;
                for p in &list {
                    *parole.entry(*p).or_default() += 1;
                }
                if back == 0 {
                    newest = list;
                }
            }
            let mut tally = Tally::new(spec.parole_dedup.is_active(number));
            let window = usize::try_from(epoch).map_err(|_| Error::Amount)?;
            scan(reader, last(window), &mut tally)?;
            for p in newest {
                let served = parole.get(&p) == Some(&limit);
                let signed = tally.counts.get(&p).copied().unwrap_or(0) >= cfg.minimum_signing_tx;
                if !(served && signed) {
                    out.push(p);
                }
            }
        }
    }
    Ok(out)
}

/// Checks the `Penalties` of an epoch-switch header against the derived list: strictly, and with an
/// error, never a pass, when the list cannot be derived (xOneGo#699; geth `verifyHeader.go:423`).
pub fn verify<R: BlockReader + PenaltyHistory>(
    spec: &ChainSpec,
    reader: &R,
    input: &Input<'_>,
    header_penalties: &[Address],
) -> Result<(), Error> {
    let derived = penalties(spec, reader, input)?;
    if same_set(&derived, header_penalties) {
        return Ok(());
    }
    Err(Error::PenaltyMismatch {
        derived,
        header: header_penalties.to_vec(),
    })
}
