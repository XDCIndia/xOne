//! The masternodes of a V2 epoch and the leader of a round.

use alloy_primitives::Address;
use xone_chainspec::V2;

use super::Error;

/// `MaxMasternodes` of the config at `round` (geth `config.V2.Config(round).MaxMasternodes`,
/// `engine.go:2025`).
pub fn max_masternodes(v2: &V2, round: u64) -> Result<usize, Error> {
    let n = v2
        .config_at_round(round)
        .ok_or(Error::NoV2Config(round))?
        .max_masternodes;
    usize::try_from(n).map_err(|_| Error::TooMany(usize::MAX))
}

/// geth `calcMasternodes` after the penalties are known (`engine.go:2095-2098`): the pool without
/// the penalised, in pool order, cut at `max`. The cut comes after the removal.
pub fn calc(pool: &[Address], penalties: &[Address], max: usize) -> Vec<Address> {
    pool.iter()
        .filter(|a| !penalties.contains(a))
        .take(max)
        .copied()
        .collect()
}

/// The first V2 block, whose epoch has no penalties (`engine.go:2051-2062`): the pool cut at `max`.
pub fn first_v2_block(pool: &[Address], max: usize) -> Vec<Address> {
    pool.iter().take(max).copied().collect()
}

/// geth `verifyHeader` and `yourturnAligned` (`verifyHeader.go:480`, `mining.go:67`): the leader of
/// `round` is `masternodes[round % epoch % len]`. The round is the absolute round, not the rounds
/// since the switch block; the list is the one of the epoch the block is in (xOneGo #675).
pub fn leader_index(round: u64, epoch: u64, len: usize) -> Result<usize, Error> {
    let n = u64::try_from(len).map_err(|_| Error::TooMany(len))?;
    let i = round
        .checked_rem(epoch)
        .ok_or(Error::ZeroEpoch)?
        .checked_rem(n)
        .ok_or(Error::NoLeader(round))?;
    usize::try_from(i).map_err(|_| Error::TooMany(len))
}

pub fn leader(round: u64, epoch: u64, masternodes: &[Address]) -> Result<Address, Error> {
    let i = leader_index(round, epoch, masternodes.len())?;
    masternodes.get(i).copied().ok_or(Error::NoLeader(round))
}
