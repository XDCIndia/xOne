//! Penalties and the comeback of penalised masternodes. Pure functions over a [`crate::chain`]
//! reader; rules in `spec/consensus/penalties-and-comeback.md`.

pub mod comeback;
pub mod v1;
pub mod v2;

use alloy_primitives::Address;

/// geth `CompareSignersLists` (`consensus/XDPoS/utils/utils.go:59`): the same addresses, the same
/// number of times, in any order.
pub fn same_set(a: &[Address], b: &[Address]) -> bool {
    let (mut a, mut b) = (a.to_vec(), b.to_vec());
    a.sort_unstable();
    b.sort_unstable();
    a == b
}
