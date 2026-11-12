//! Verification of `eth_getProof` answers (EIP-1186) against a state root the caller already trusts:
//! the account against the state root of a verified header, a slot against the storage root of the
//! account just proven. A source that forges a field or a node fails here, so it need not be trusted.
//! Used for the 0x88 state of a gap block when no xOne peer serves it over snap/1.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_trie::{proof::verify_proof, Nibbles};
use xone_state::Account;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RpcProofError {
    #[error("account proof of {0} does not verify against the state root")]
    Account(Address),
    #[error("storage proof of slot {0} does not verify against the storage root")]
    Slot(B256),
}

/// `account` is what the source claims (`None`: absent). The proof is the node list root first.
pub fn verify_account(
    root: B256,
    address: Address,
    account: Option<&Account>,
    proof: &[Bytes],
) -> Result<(), RpcProofError> {
    verify_proof(
        root,
        Nibbles::unpack(keccak256(address)),
        account.map(Account::rlp),
        proof,
    )
    .map_err(|_| RpcProofError::Account(address))
}

/// A zero value is an exclusion proof, as in the trie (zero slots are absent).
pub fn verify_slot(
    storage_root: B256,
    slot: B256,
    value: U256,
    proof: &[Bytes],
) -> Result<(), RpcProofError> {
    verify_proof(
        storage_root,
        Nibbles::unpack(keccak256(slot)),
        (!value.is_zero()).then(|| alloy_rlp::encode(value)),
        proof,
    )
    .map_err(|_| RpcProofError::Slot(slot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpt::Trie;
    use std::collections::BTreeMap;

    #[test]
    fn a_proof_verifies_and_a_forged_field_or_node_does_not() {
        let mut leaves = BTreeMap::new();
        let mut accs = Vec::new();
        for i in 0..40u8 {
            let a = Address::repeat_byte(i + 1);
            let x = Account {
                nonce: u64::from(i),
                ..Default::default()
            };
            leaves.insert(keccak256(a), x.rlp());
            accs.push((a, x));
        }
        let t = Trie::build(&leaves);
        let (a, x) = accs[7].clone();
        let proof = t.prove(keccak256(a));
        assert_eq!(verify_account(t.root, a, Some(&x), &proof), Ok(()));
        // a forged field
        let mut y = x.clone();
        y.nonce += 1;
        assert!(verify_account(t.root, a, Some(&y), &proof).is_err());
        // a forged node
        let mut bad = proof.clone();
        let last = bad.len() - 1;
        let mut b = bad[last].to_vec();
        let n = b.len() - 1;
        b[n] ^= 1;
        bad[last] = b.into();
        assert!(verify_account(t.root, a, Some(&x), &bad).is_err());
        // an absent account is proven absent, and cannot be claimed present
        let ghost = Address::repeat_byte(0xee);
        let p = t.prove(keccak256(ghost));
        assert_eq!(verify_account(t.root, ghost, None, &p), Ok(()));
        assert!(verify_account(t.root, ghost, Some(&x), &p).is_err());
        // another root
        assert!(verify_account(B256::repeat_byte(9), a, Some(&x), &proof).is_err());
    }

    #[test]
    fn slots_verify_and_zero_is_an_exclusion() {
        let mut leaves = BTreeMap::new();
        for i in 1..30u64 {
            leaves.insert(
                keccak256(B256::from(U256::from(i))),
                alloy_rlp::encode(U256::from(i * 7)),
            );
        }
        let t = Trie::build(&leaves);
        let s = B256::from(U256::from(5));
        let p = t.prove(keccak256(s));
        assert_eq!(verify_slot(t.root, s, U256::from(35), &p), Ok(()));
        assert!(verify_slot(t.root, s, U256::from(36), &p).is_err());
        assert!(verify_slot(t.root, s, U256::ZERO, &p).is_err());
        let z = B256::from(U256::from(999));
        assert_eq!(
            verify_slot(t.root, z, U256::ZERO, &t.prove(keccak256(z))),
            Ok(())
        );
    }
}
