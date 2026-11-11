//! The masternode list of a V1 checkpoint header (every `Epoch` blocks).
//!
//! Where the list comes from, in geth-XDC order:
//! 1. at the gap block (`number % epoch == epoch - gap`), `UpdateMasternodesV1` reads every
//!    candidate of the 0x88 contract with its cap from the state after that block, sorts them by
//!    cap with the frozen quicksort, keeps the first `maxMasternodes` and stores them as the
//!    snapshot's signer set (`consensus/XDPoS/xdpos.go:2135-2211`);
//! 2. at the checkpoint, `Prepare` takes `snap.GetSigners()` (ascending by address,
//!    `consensus/XDPoS/snapshot.go:249-263`), removes the penalties of this epoch and then those
//!    kept in the `Penalties` field of the checkpoints of the previous `LimitPenaltyEpoch` epochs
//!    (`xdpos.go:1439-1476`) and writes the rest into `extraData` in that order.
//!
//! So the cap decides which candidates are in; the order in the header is by address.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, U256};
use xone_primitives::EXTRA_SEAL;

use super::gosort::sort_slice;
use crate::verify::HeaderError;

/// Length of the vanity prefix of `extraData` (geth `extraVanity`).
pub const EXTRA_VANITY: usize = 32;
/// geth `MaxMasternodes` (`consensus/XDPoS/constants.go:41`).
pub const MAX_MASTERNODES: u64 = 18;
/// geth `LimitPenaltyEpoch` (`consensus/XDPoS/constants.go:44`).
pub const LIMIT_PENALTY_EPOCH: u64 = 4;

/// geth `extractAddressFromBytes` (`xdpos.go:2720`) without its all-or-nothing length check: a
/// trailing partial address is dropped, as `contracts.ExtractAddressFromBytes` does.
pub fn addresses(bytes: &[u8]) -> Vec<Address> {
    bytes
        .chunks_exact(Address::len_bytes())
        .map(Address::from_slice)
        .collect()
}

/// geth `compareSignersLists` (`xdpos.go:2687`): equal as multisets.
pub fn same_set(a: &[Address], b: &[Address]) -> bool {
    let (mut a, mut b) = (a.to_vec(), b.to_vec());
    a.sort();
    b.sort();
    a == b
}

/// The masternodes in the `extraData` of a V1 checkpoint (`GetMasternodesFromCheckpointHeader`,
/// `xdpos.go:2122-2127`): between the 32 byte vanity and the 65 byte seal.
pub fn masternodes_of(extra: &[u8]) -> Result<Vec<Address>, HeaderError> {
    let end = extra
        .len()
        .checked_sub(EXTRA_SEAL)
        .ok_or(HeaderError::MissingSignature)?;
    let list = extra
        .get(EXTRA_VANITY..end)
        .ok_or(HeaderError::MissingVanity)?;
    Ok(addresses(list))
}

/// geth `UpdateMasternodes` (`xdpos.go:2163-2174`): the limit of the gap block `gap_number`.
/// `max_v2` is `XDPoS.maxMasternodesV2` of the chain, else the network constant.
pub fn max_masternodes(
    gap_number: u64,
    tip_increase: Option<u64>,
    switch_block: Option<u64>,
    max_v2: u64,
) -> u64 {
    let raised = tip_increase.is_some_and(|t| gap_number >= t)
        || switch_block.is_some_and(|s| gap_number > s);
    if raised {
        max_v2
    } else {
        MAX_MASTERNODES
    }
}

/// The candidates `UpdateMasternodesV1` keeps, in cap order: sorted by cap, largest first, with
/// geth's frozen comparator `Cmp(...) >= 0` (`xdpos.go:2148-2150`), cut at `max`. No candidate is
/// a typed error (`xdpos.go:2143`).
pub fn select_candidates(
    candidates: &[(Address, U256)],
    max: u64,
) -> Result<Vec<Address>, HeaderError> {
    if candidates.is_empty() {
        return Err(HeaderError::MissingCandidates);
    }
    let mut sorted = candidates.to_vec();
    sort_slice(&mut sorted, |a, b| a.1 >= b.1);
    Ok(sorted
        .into_iter()
        .take(usize::try_from(max).unwrap_or(usize::MAX))
        .map(|(a, _)| a)
        .collect())
}

/// The masternodes a checkpoint header must carry, in header order: the candidates kept by
/// [`select_candidates`], minus `penalties`, ascending by address. `penalties` is
/// [`checkpoint_penalties`]. An empty result is a typed error.
pub fn checkpoint_masternodes(
    candidates: &[(Address, U256)],
    max_masternodes: u64,
    penalties: &[Address],
) -> Result<Vec<Address>, HeaderError> {
    let mut set: BTreeSet<Address> = select_candidates(candidates, max_masternodes)?
        .into_iter()
        .collect();
    for p in penalties {
        set.remove(p);
    }
    if set.is_empty() {
        return Err(HeaderError::EmptyCheckpointList);
    }
    Ok(set.into_iter().collect())
}

/// Everything removed from the snapshot's signers at checkpoint `number`: `current`, the penalties
/// of this epoch, then the `Penalties` of checkpoints `number - i * epoch`, `i = 1..=4` while
/// `number > i * epoch` (`xdpos.go:1468-1472`). `previous` maps a checkpoint number to its
/// penalties. geth skips a checkpoint it cannot find (`removePenaltiesFromBlock`, `xdpos.go:2622-2624`);
/// here it is [`HeaderError::MissingPenaltyHeader`].
pub fn checkpoint_penalties(
    number: u64,
    epoch: u64,
    current: &[Address],
    previous: &BTreeMap<u64, Vec<Address>>,
) -> Result<Vec<Address>, HeaderError> {
    let mut out = current.to_vec();
    for i in 1..=LIMIT_PENALTY_EPOCH {
        let back = epoch.saturating_mul(i);
        if number > back {
            let key = number - back;
            out.extend_from_slice(
                previous
                    .get(&key)
                    .ok_or(HeaderError::MissingPenaltyHeader(key))?,
            );
        }
    }
    Ok(out)
}

/// A checkpoint header's list against the one derived by [`checkpoint_masternodes`]. A different
/// set (dropped or extra address) is geth's `errInvalidCheckpointSigners`. The same set in another
/// order is [`HeaderError::CheckpointOrder`]: geth compares as a multiset (`xdpos.go:1125`), so the
/// order is the rule of the producer, which `HeaderVerifier` does not enforce.
pub fn verify_checkpoint_list(listed: &[Address], expected: &[Address]) -> Result<(), HeaderError> {
    if !same_set(listed, expected) {
        return Err(HeaderError::InvalidCheckpointSigners);
    }
    if listed != expected {
        return Err(HeaderError::CheckpointOrder);
    }
    Ok(())
}

/// geth `checkSignersOnCheckpoint` (`xdpos.go:1069-1076`): `Penalties` of the header equals the
/// computed list byte for byte.
pub fn verify_checkpoint_penalties(
    header_penalties: &[u8],
    computed: &[Address],
) -> Result<(), HeaderError> {
    let bytes: Vec<u8> = computed.iter().flat_map(|a| a.0).collect();
    if header_penalties != bytes {
        return Err(HeaderError::InvalidCheckpointPenalties);
    }
    Ok(())
}
