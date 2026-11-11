//! The XDC account leaf (geth-XDC `StateAccount`, `core/types/gen_account_rlp.go:14-40`) and its
//! slim form for snap (`core/types/state_account.go:89-135`).

use alloy_primitives::{B256, KECCAK256_EMPTY};
use alloy_rlp::{Decodable, Encodable, Header, RlpDecodable, RlpEncodable};
use alloy_trie::EMPTY_ROOT_HASH;
use xone_primitives::WideBalance;

/// `[nonce, balance, storage_root, code_hash]`. The balance is the true integer, any width up to
/// [`xone_primitives::MAX_WIDE_BITS`]; the RLP item is a plain big integer.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Account {
    pub nonce: u64,
    pub balance: WideBalance,
    pub storage_root: B256,
    pub code_hash: B256,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            nonce: 0,
            balance: WideBalance::default(),
            storage_root: EMPTY_ROOT_HASH,
            code_hash: KECCAK256_EMPTY,
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SlimError {
    #[error("rlp: {0}")]
    Rlp(#[from] alloy_rlp::Error),
    /// geth reads these two items as free-length byte strings. A length other than 0 or 32
    /// cannot come from `SlimAccountRLP`; it is refused here instead of padded or cut.
    #[error("slim root or code hash is neither empty nor 32 bytes")]
    BadHashLength,
}

impl Account {
    /// The trie leaf value (`StateAccount.EncodeRLP`).
    pub fn rlp(&self) -> Vec<u8> {
        alloy_rlp::encode(self)
    }

    /// Snap form (`SlimAccountRLP`): the empty storage root and the empty code hash become the
    /// empty string. The balance item is unchanged, so a wide balance goes through in full.
    pub fn slim_rlp(&self) -> Vec<u8> {
        let mut p = Vec::new();
        self.nonce.encode(&mut p);
        self.balance.encode(&mut p);
        if self.storage_root == EMPTY_ROOT_HASH {
            [].as_slice().encode(&mut p);
        } else {
            self.storage_root.encode(&mut p);
        }
        if self.code_hash == KECCAK256_EMPTY {
            [].as_slice().encode(&mut p);
        } else {
            self.code_hash.encode(&mut p);
        }
        let mut out = Vec::with_capacity(p.len() + 3);
        Header {
            list: true,
            payload_length: p.len(),
        }
        .encode(&mut out);
        out.extend_from_slice(&p);
        out
    }

    /// `FullAccount`: the inverse of [`Account::slim_rlp`].
    pub fn from_slim_rlp(mut buf: &[u8]) -> Result<Self, SlimError> {
        let b = &mut buf;
        let h = Header::decode(b)?;
        if !h.list || h.payload_length != b.len() {
            return Err(alloy_rlp::Error::UnexpectedLength.into());
        }
        let nonce = u64::decode(b)?;
        let balance = WideBalance::decode(b)?;
        let hash = |b: &mut &[u8], empty: B256| match Header::decode_bytes(b, false)? {
            [] => Ok(empty),
            x if x.len() == 32 => Ok(B256::from_slice(x)),
            _ => Err(SlimError::BadHashLength),
        };
        let storage_root = hash(b, EMPTY_ROOT_HASH)?;
        let code_hash = hash(b, KECCAK256_EMPTY)?;
        if !b.is_empty() {
            return Err(alloy_rlp::Error::UnexpectedLength.into());
        }
        Ok(Self {
            nonce,
            balance,
            storage_root,
            code_hash,
        })
    }
}
