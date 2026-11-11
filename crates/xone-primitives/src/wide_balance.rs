//! Account balance as geth-XDC stores it: the true value plus its wrapped low part.
//!
//! geth-XDC keeps `StateAccount.Balance` (uint256, the value mod 2^256) and
//! `BigBalance` (the true integer, set only above 2^256-1). The EVM sees only the
//! wrapped part (`GetBalance`, so BALANCE, SELFBALANCE and SELFDESTRUCT);
//! the trie leaf carries the true integer as an unbounded RLP big integer
//! (core/types/gen_account_rlp.go, state_account.go).

use alloy_primitives::U256;
use alloy_rlp::{BufMut, Decodable, Encodable, Error as RlpError, Header};
use num_bigint::BigUint;

/// Largest legal balance is 2^MAX_WIDE_BITS - 1. Genesis alone funds at most nine
/// accounts with 2^256-1, so supply is below 2^260; this bound keeps one extra
/// RLP byte of room and is enforced as a typed error, never a wrap.
pub const MAX_WIDE_BITS: u64 = 264;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WideBalanceError {
    #[error("balance would be negative")]
    Underflow,
    #[error("balance above 2^{MAX_WIDE_BITS}-1")]
    AboveBound,
}

/// True balance, arbitrary precision. Codec fidelity with geth is unbounded;
/// the legal range is enforced by [`WideBalance::checked_add`] and
/// [`WideBalance::check_legal`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct WideBalance(BigUint);

impl From<U256> for WideBalance {
    fn from(v: U256) -> Self {
        Self::from_be_bytes(&v.to_be_bytes::<32>())
    }
}

impl WideBalance {
    pub fn from_be_bytes(bytes: &[u8]) -> Self {
        Self(BigUint::from_bytes_be(bytes))
    }

    /// Minimal big-endian bytes; zero is the empty slice, as RLP writes it.
    pub fn to_be_bytes(&self) -> Vec<u8> {
        if self.0.bits() == 0 {
            Vec::new()
        } else {
            self.0.to_bytes_be()
        }
    }

    /// The value mod 2^256: what BALANCE, SELFBALANCE and SELFDESTRUCT see in geth.
    pub fn low(&self) -> U256 {
        let le = self.0.to_bytes_le();
        U256::from_le_slice(&le[..le.len().min(32)])
    }

    /// True when the value does not fit in 256 bits (geth: `BigBalance != nil`).
    pub fn is_wide(&self) -> bool {
        self.0.bits() > 256
    }

    pub fn check_legal(&self) -> Result<(), WideBalanceError> {
        if self.0.bits() > MAX_WIDE_BITS {
            return Err(WideBalanceError::AboveBound);
        }
        Ok(())
    }

    pub fn checked_add(&self, amount: U256) -> Result<Self, WideBalanceError> {
        let sum = Self(&self.0 + BigUint::from_bytes_be(&amount.to_be_bytes::<32>()));
        sum.check_legal()?;
        Ok(sum)
    }

    pub fn checked_sub(&self, amount: U256) -> Result<Self, WideBalanceError> {
        let amount = BigUint::from_bytes_be(&amount.to_be_bytes::<32>());
        if amount > self.0 {
            return Err(WideBalanceError::Underflow);
        }
        Ok(Self(&self.0 - amount))
    }

    /// `Some` when the value fits in 256 bits.
    pub fn to_u256(&self) -> Option<U256> {
        (!self.is_wide()).then(|| self.low())
    }
}

impl Encodable for WideBalance {
    fn encode(&self, out: &mut dyn BufMut) {
        self.to_be_bytes().as_slice().encode(out);
    }

    fn length(&self) -> usize {
        self.to_be_bytes().as_slice().length()
    }
}

impl Decodable for WideBalance {
    /// Any length, as geth `rlp.DecodeBytes` into a `big.Int`; leading zeros are
    /// rejected as geth does ("non-canonical integer").
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let payload = Header::decode_bytes(buf, false)?;
        if payload.first() == Some(&0) {
            return Err(RlpError::LeadingZero);
        }
        Ok(Self::from_be_bytes(payload))
    }
}
