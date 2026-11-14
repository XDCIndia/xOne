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

/// `transfer(address,uint256)`.
const TRANSFER: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];
/// `transferFrom(address,address,uint256)`.
const TRANSFER_FROM: [u8; 4] = [0x23, 0xb8, 0x72, 0xdd];

/// The `holder` balance a token stores.
pub fn token_balance(s: &impl StateRead, token: Address, holder: Address) -> U256 {
    s.storage(token, balance_slot(holder))
}

/// geth-XDC `ValidateTRC21Tx` (`core/state/trc21_reader.go:106-133`): may `from` send this
/// token-fee transaction? Calldata shorter than a selector: no. A zero token balance: yes (the
/// reader's "balance = 0 and fee = 0" path). Otherwise the balance must cover `minFee` plus the
/// value of a `transfer` (exactly 68 bytes, value at 36..68) or `transferFrom` (exactly 80 bytes,
/// value at 68..80, as the reference packs it); any other call adds no value.
pub fn validate_trc21_tx(s: &impl StateRead, from: Address, token: Address, data: &[u8]) -> bool {
    if data.len() < 4 {
        return false;
    }
    let balance = token_balance(s, token, from);
    if balance.is_zero() {
        return true;
    }
    let min_fee = s.storage(token, min_fee_slot());
    let value = match (&data[..4], data.len()) {
        (sel, 68) if sel == TRANSFER => U256::from_be_slice(&data[36..]),
        (sel, 80) if sel == TRANSFER_FROM => U256::from_be_slice(&data[68..]),
        _ => U256::ZERO,
    };
    min_fee
        .checked_add(value)
        .is_some_and(|need| balance >= need)
}

#[cfg(test)]
mod pool_tests {
    use super::*;
    use crate::Account;

    struct St(BTreeMap<(Address, B256), U256>);
    impl StateRead for St {
        fn account(&self, _: Address) -> Option<Account> {
            None
        }
        fn storage(&self, a: Address, k: B256) -> U256 {
            self.0.get(&(a, k)).copied().unwrap_or(U256::ZERO)
        }
    }

    #[test]
    fn validate_follows_the_reference_reader() {
        let (token, from) = (Address::repeat_byte(7), Address::repeat_byte(8));
        let transfer = |v: u64| {
            let mut d = TRANSFER.to_vec();
            d.extend_from_slice(&[0u8; 32]);
            d.extend_from_slice(&U256::from(v).to_be_bytes::<32>());
            d
        };
        let empty = St(BTreeMap::new());
        assert!(!validate_trc21_tx(&empty, from, token, &[0xa9]));
        // no token balance: accepted
        assert!(validate_trc21_tx(&empty, from, token, &transfer(1_000)));
        let st = St(BTreeMap::from([
            ((token, balance_slot(from)), U256::from(100)),
            ((token, min_fee_slot()), U256::from(10)),
        ]));
        assert!(validate_trc21_tx(&st, from, token, &transfer(90)));
        assert!(!validate_trc21_tx(&st, from, token, &transfer(91)));
        // another call: only minFee
        assert!(validate_trc21_tx(&st, from, token, &[1, 2, 3, 4]));
        assert_eq!(token_balance(&st, token, from), U256::from(100));
    }
}
