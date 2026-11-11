//! Who may create the next V1 block: the in-turn rule, the difficulty that records it, and the
//! creator checks of geth `verifySeal` (`consensus/XDPoS/xdpos.go:1235-1322`).

use alloy_primitives::Address;

use crate::verify::HeaderError;

fn index(list: &[Address], x: Address) -> i64 {
    list.iter()
        .position(|a| *a == x)
        .and_then(|i| i64::try_from(i).ok())
        .unwrap_or(-1)
}

/// geth `Hop` (`consensus/XDPoS/utils/utils.go:25`, `xdpos.go:2658`): the distance in turns
/// between two positions of the masternode list; `-1` is "not in the list".
pub fn hop(len: i64, pre: i64, cur: i64) -> i64 {
    match pre.cmp(&cur) {
        std::cmp::Ordering::Less => cur - (pre + 1),
        std::cmp::Ordering::Greater => (len - pre) + (cur - 1),
        std::cmp::Ordering::Equal => len - 1,
    }
}

/// `(len, pre, cur)` of geth `YourTurn` (`xdpos.go:1983-2011`): the masternode count and the
/// positions of the parent's creator and of `signer`. The parent's creator is only absent when
/// the parent is the genesis block (position -1); otherwise it must be given.
fn positions(
    masternodes: &[Address],
    parent_number: u64,
    parent_creator: Option<Address>,
    signer: Address,
) -> Result<(i64, i64, i64), HeaderError> {
    if masternodes.is_empty() {
        return Err(HeaderError::MissingMasternodes);
    }
    let len = i64::try_from(masternodes.len()).map_err(|_| HeaderError::MissingMasternodes)?;
    let pre = match (parent_number, parent_creator) {
        (0, _) => -1,
        (_, Some(p)) => index(masternodes, p),
        (_, None) => return Err(HeaderError::MissingParentCreator),
    };
    Ok((len, pre, index(masternodes, signer)))
}

/// geth `YourTurn` before the V2 switch (`xdpos.go:2022-2024`): the masternode right after the
/// parent's creator is in turn.
pub fn in_turn(
    masternodes: &[Address],
    parent_number: u64,
    parent_creator: Option<Address>,
    signer: Address,
) -> Result<bool, HeaderError> {
    let (len, pre, cur) = positions(masternodes, parent_number, parent_creator, signer)?;
    Ok((pre + 1) % len == cur)
}

/// geth `calcDifficulty` (`xdpos.go:1875-1881`): `len - Hop(len, pre, cur)`. It is at least 1; the in-turn
/// creator carries `len`, and each place further from the parent's creator carries one less.
pub fn difficulty(
    masternodes: &[Address],
    parent_number: u64,
    parent_creator: Option<Address>,
    creator: Address,
) -> Result<i64, HeaderError> {
    let (len, pre, cur) = positions(masternodes, parent_number, parent_creator, creator)?;
    Ok(len - hop(len, pre, cur))
}

/// geth `verifySeal` difficulty check (`xdpos.go:1267-1272`).
pub fn check_difficulty(have: i64, want: i64) -> Result<(), HeaderError> {
    if have != want {
        return Err(HeaderError::InvalidDifficulty { have, want });
    }
    Ok(())
}

/// Seconds an out-of-turn masternode `hops` places after the parent's creator waits before it
/// may seal (`xdpos.go:2028-2033`): 10 per hop, 20 per hop when the hops reach the next
/// checkpoint. A producer rule; verification does not read the clock.
pub fn out_of_turn_wait(hops: u64, parent_number: u64, epoch: u64) -> Result<u64, HeaderError> {
    let to_checkpoint = epoch
        - parent_number
            .checked_rem(epoch)
            .ok_or(HeaderError::ZeroEpoch)?;
    let per_hop = if hops >= to_checkpoint { 20 } else { 10 };
    Ok(per_hop * hops)
}

/// geth `verifySeal` creator checks (`xdpos.go:1296-1322`): the creator is in the snapshot's
/// signers or, failing that, among the `masternodes` of the block; and with more than one
/// masternode, outside a checkpoint, it is not the creator of the parent (`recent_creator`,
/// the newest entry of the snapshot's recents, the only one within two blocks).
pub fn authorize(
    creator: Address,
    signers: &[Address],
    masternodes: &[Address],
    recent_creator: Option<Address>,
    number: u64,
    epoch: u64,
) -> Result<(), HeaderError> {
    if !signers.contains(&creator) && !masternodes.contains(&creator) {
        return Err(HeaderError::Unauthorized(creator));
    }
    let checkpoint = number.checked_rem(epoch).ok_or(HeaderError::ZeroEpoch)? == 0;
    if masternodes.len() > 1 && !checkpoint && recent_creator == Some(creator) {
        return Err(HeaderError::Unauthorized(creator));
    }
    Ok(())
}
