//! The V2 checkpoint reward before the tiered rewards: the window comes from the epoch switches,
//! the payout is the V1 flat one (geth `GetSigningTxCount`, `eth/hooks/engine_v2_hooks.go:621`).
//!
//! At an epoch switch the epoch that just ended is not the one rewarded: the walk back from the
//! switch finds the switch of the epoch that ended (`end` is the block before it) and the one
//! before (`start` is the block after it, and its header lists the masternodes). The V2 switch
//! block plus one is never counted as a switch (`engine_v2_hooks.go:700`).

use alloy_primitives::B256;
use xone_chainspec::ChainSpec;

use super::{Holders, RewardMap, RewardWindow};
use crate::chain::{header_at, BlockReader, Error};

/// The rewarded blocks of an epoch switch and the switch whose header lists the masternodes (and,
/// from the tiered rewards, the standby pool).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Located {
    pub start: u64,
    pub end: u64,
    pub switch: (B256, u64),
}

/// Walks the parent chain of the checkpoint block `number` from `parent` = (hash, number - 1),
/// as `engine_v2_hooks.go:680-720` and `:949-995` do. Fails with [`Error::WindowNotFound`] where
/// geth would run off genesis.
pub fn locate<R: BlockReader>(
    spec: &ChainSpec,
    reader: &R,
    number: u64,
    parent: B256,
) -> Result<Located, Error> {
    let switch_plus_one = spec
        .xdpos
        .v2
        .as_ref()
        .ok_or(Error::NoV2)?
        .switch_block
        .saturating_add(1);
    let (mut hash, mut at, mut found, mut end) = (parent, number, 0, 0);
    while at > 0 {
        at -= 1;
        let h = header_at(reader, hash, at)?;
        if h.epoch_switch && at != switch_plus_one {
            found += 1;
            if found == 1 {
                end = at.saturating_sub(1);
            }
            if found == 2 {
                return Ok(Located {
                    start: at + 1,
                    end,
                    switch: (hash, at),
                });
            }
        }
        hash = h.parent_hash;
    }
    Err(Error::WindowNotFound(number))
}

/// The reward map of V2 checkpoint `number` before the tiered rewards. Empty at the V2 switch
/// block plus one (`engine_v2_hooks.go:76`).
pub fn rewards(
    spec: &ChainSpec,
    number: u64,
    window: &RewardWindow,
    holders: &Holders,
    parent_root: B256,
) -> Result<RewardMap, Error> {
    let v2 = spec.xdpos.v2.as_ref().ok_or(Error::NoV2)?;
    if number == v2.switch_block.saturating_add(1) {
        return Ok(RewardMap::new());
    }
    super::v1::flat(spec, number, window, holders, parent_root, false)
}
