//! The validator contract at 0x88 (geth `common.MasternodeVotingSMCBinary`, `common/xdc_contracts.go:23`).
//!
//! Slots are `slotValidatorMapping`, `core/state/statedb_utils.go:75-93`: `validatorsState` is the
//! mapping at slot 1 whose value is the struct `{address owner; uint256 cap; mapping voters}`
//! (owner at +0, cap at +1, voters mapping at +2, `statedb_utils.go:114-157`); `voters` is the
//! mapping to an address array at slot 2; `candidates` is the address array at slot 8.

use super::{address_of, dyn_array_slot, key, mapping_slot};
use crate::StateRead;
use alloy_primitives::{address, keccak256, Address, B256, U256};

/// geth `common.MasternodeVotingSMCBinary`, `common/xdc_contracts.go:23`.
pub const ADDRESS: Address = address!("0000000000000000000000000000000000000088");

/// `slotValidatorMapping["validatorsState"]`, `statedb_utils.go:77`.
pub const VALIDATORS_STATE: u64 = 1;
/// `slotValidatorMapping["voters"]`, `statedb_utils.go:78`.
pub const VOTERS: u64 = 2;
/// `slotValidatorMapping["candidates"]`, `statedb_utils.go:84`.
pub const CANDIDATES: u64 = 8;

fn read(s: &impl StateRead, slot: U256) -> U256 {
    s.storage(ADDRESS, B256::from(slot.to_be_bytes::<32>()))
}

/// Candidates in contract order, empty entries skipped (geth `GetCandidates`, `statedb_utils.go:95-112`).
pub fn candidates(s: &impl StateRead) -> Vec<Address> {
    let slot = U256::from(CANDIDATES);
    let n = read(s, slot).saturating_to::<u64>();
    (0..n)
        .map(|i| read(s, dyn_array_slot(slot, i, 1)))
        .filter(|w| !w.is_zero())
        .map(address_of)
        .collect()
}

/// `validatorsState[candidate].owner`; `None` when the word is the zero address, which geth's
/// fee routing treats as "no owner" (`core/state_transition.go:900`). geth `GetCandidateOwner`,
/// `statedb_utils.go:114-121`.
pub fn candidate_owner(s: &impl StateRead, candidate: Address) -> Option<Address> {
    let o = address_of(read(s, mapping_slot(key(candidate), VALIDATORS_STATE)));
    (!o.is_zero()).then_some(o)
}

/// `validatorsState[candidate].cap` (geth `GetCandidateCap`, `statedb_utils.go:123-130`).
pub fn candidate_cap(s: &impl StateRead, candidate: Address) -> U256 {
    read(
        s,
        mapping_slot(key(candidate), VALIDATORS_STATE).wrapping_add(U256::from(1)),
    )
}

/// `voters[candidate]`, the voters of a candidate in contract order (geth `GetVoters`,
/// `statedb_utils.go:132-149`).
pub fn voters(s: &impl StateRead, candidate: Address) -> Vec<Address> {
    let slot = mapping_slot(key(candidate), VOTERS);
    let n = read(s, slot).saturating_to::<u64>();
    (0..n)
        .map(|i| address_of(read(s, dyn_array_slot(slot, i, 1))))
        .collect()
}

/// `validatorsState[candidate].voters[voter]`, the stake of one voter (geth `GetVoterCap`,
/// `statedb_utils.go:151-157`): the inner mapping lives at `validatorsState[candidate] + 2`.
pub fn voter_cap(s: &impl StateRead, candidate: Address, voter: Address) -> U256 {
    let base = mapping_slot(key(candidate), VALIDATORS_STATE).wrapping_add(U256::from(2));
    let mut b = [0u8; 64];
    b[..32].copy_from_slice(key(voter).as_slice());
    b[32..].copy_from_slice(&base.to_be_bytes::<32>());
    read(s, U256::from_be_bytes(keccak256(b).0))
}
