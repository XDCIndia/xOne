//! The TRC21 issuer contract and the token storage the token-fee path touches (geth
//! `core/state/trc21_reader.go:26-91`, `:135-148`; the contract address `common/types.go:124`, `:213`,
//! `:229`).
//!
//! A token registered in the issuer has an XDC deposit there, its fee capacity: `tokens` is the
//! address array at slot 1, `tokensState` the mapping token to capacity at slot 2. A token keeps
//! `balances` (mapping at slot 0), `minFee` (slot 1) and `issuer` (slot 2). The slot arithmetic is
//! that of the other system contracts, [`super::mapping_slot`] and [`super::dyn_array_slot`].

use super::{address_of, dyn_array_slot, key, mapping_slot, word};
use crate::StateRead;
use alloy_primitives::{address, Address, B256, U256};
use std::collections::BTreeMap;

/// `slotTRC21IssuerTokens`, `trc21_reader.go:42`.
const TOKENS: u64 = 1;
/// `slotTRC21IssuerTokensState`, `trc21_reader.go:43`.
const TOKENS_STATE: u64 = 2;
/// `slotTRC21TokenBalances`, `trc21_reader.go:44`.
const TOKEN_BALANCES: u64 = 0;
/// `slotTRC21TokenMinFee`, `trc21_reader.go:45`.
const TOKEN_MIN_FEE: u64 = 1;
/// `slotTRC21TokenIssuer`, `trc21_reader.go:46`.
const TOKEN_ISSUER: u64 = 2;

/// The issuer of a chain: Apothem has its own (`CopyXDCConstants`, `common/types.go:213`), every
/// other chain this crate knows has the package default (`common/types.go:124`, `:229`).
pub fn issuer(chain_id: u64) -> Address {
    if chain_id == 51 {
        address!("0E2C88753131CE01c7551B726b28BFD04e44003F")
    } else {
        address!("8c0faeb5C6bEd2129b8674F262Fd45c4e9468bee")
    }
}

/// Storage key of `tokensState[token]` in the issuer.
pub fn capacity_slot(token: Address) -> B256 {
    word(mapping_slot(key(token), TOKENS_STATE))
}

/// Storage key of `balances[holder]` in a token.
pub fn balance_slot(holder: Address) -> B256 {
    word(mapping_slot(key(holder), TOKEN_BALANCES))
}

/// Storage key of the token's `minFee`.
pub fn min_fee_slot() -> B256 {
    word(U256::from(TOKEN_MIN_FEE))
}

/// Storage key of the token's `issuer`.
pub fn token_issuer_slot() -> B256 {
    word(U256::from(TOKEN_ISSUER))
}

/// The address a token stores as its issuer (the low 20 bytes of the word).
pub fn token_issuer(word: U256) -> Address {
    address_of(word)
}

/// geth `GetTRC21FeeCapacityFromState` (`trc21_reader.go:52-66`): the fee capacity of every token
/// registered in the issuer. An empty array entry is skipped; a token listed twice keeps one entry.
pub fn capacities(s: &impl StateRead, issuer: Address) -> BTreeMap<Address, U256> {
    let slot = U256::from(TOKENS);
    let n = s.storage(issuer, word(slot)).as_limbs()[0];
    let mut out = BTreeMap::new();
    for i in 0..n {
        let w = s.storage(issuer, word(dyn_array_slot(slot, i, 1)));
        if w.is_zero() {
            continue;
        }
        let token = address_of(w);
        out.insert(token, s.storage(issuer, capacity_slot(token)));
    }
    out
}
