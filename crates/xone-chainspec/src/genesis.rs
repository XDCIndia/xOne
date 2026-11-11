//! Genesis loading, state root and genesis header (geth `toBlockWithRoot`).

use std::{collections::BTreeMap, str::FromStr};

use alloy_primitives::{keccak256, Address, Bytes, B256, KECCAK256_EMPTY, U256};
use alloy_trie::{
    root::{state_root_unhashed, storage_root_unhashed},
    TrieAccount, EMPTY_ROOT_HASH,
};
use serde::Deserialize;
use xone_primitives::{BlockNonce, Header};

use crate::{ChainSpec, Error};

/// geth `params.GenesisGasLimit`, used when the genesis gas limit is 0.
const GENESIS_GAS_LIMIT: u64 = 4_712_388;
/// geth `params.InitialBaseFee`.
const INITIAL_BASE_FEE: u64 = 1_000_000_000;
/// keccak256(rlp([])): the uncle hash of a block without uncles.
const EMPTY_UNCLE_HASH: B256 =
    alloy_primitives::b256!("1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347");

#[derive(Debug, Clone, PartialEq)]
pub struct Genesis {
    pub nonce: u64,
    pub timestamp: u64,
    pub extra_data: Bytes,
    pub gas_limit: u64,
    pub difficulty: U256,
    pub mix_hash: B256,
    pub coinbase: Address,
    pub number: u64,
    pub gas_used: u64,
    pub parent_hash: B256,
    pub base_fee: Option<U256>,
    pub alloc: BTreeMap<Address, GenesisAccount>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GenesisAccount {
    pub nonce: u64,
    pub balance: U256,
    pub code: Bytes,
    pub storage: BTreeMap<B256, B256>,
}

// Raw JSON shapes. Quantities and addresses are strings so that hex (`0x`),
// decimal and the XDC `xdc` address prefix are all accepted.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawGenesis {
    nonce: String,
    timestamp: String,
    extra_data: Bytes,
    gas_limit: String,
    difficulty: String,
    mix_hash: B256,
    coinbase: String,
    alloc: BTreeMap<String, RawAccount>,
    number: String,
    gas_used: String,
    parent_hash: B256,
    base_fee_per_gas: Option<String>,
}

#[derive(Deserialize)]
struct RawAccount {
    #[serde(default)]
    balance: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    code: Option<Bytes>,
    #[serde(default)]
    storage: BTreeMap<B256, B256>,
}

fn quantity(field: &'static str, s: &str) -> Result<U256, Error> {
    U256::from_str(s).map_err(|_| Error::InvalidValue {
        field,
        value: s.to_owned(),
    })
}

fn quantity_u64(field: &'static str, s: &str) -> Result<u64, Error> {
    u64::try_from(quantity(field, s)?).map_err(|_| Error::InvalidValue {
        field,
        value: s.to_owned(),
    })
}

/// `0x…`, `xdc…` or bare 40 hex digits.
fn address(s: &str) -> Result<Address, Error> {
    let hex = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("xdc"))
        .unwrap_or(s);
    Address::from_str(hex).map_err(|_| Error::InvalidValue {
        field: "address",
        value: s.to_owned(),
    })
}

impl Genesis {
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let raw: RawGenesis = serde_json::from_str(json)?;
        let mut alloc = BTreeMap::new();
        for (addr, a) in raw.alloc {
            let account = GenesisAccount {
                nonce: a
                    .nonce
                    .as_deref()
                    .map_or(Ok(0), |n| quantity_u64("nonce", n))?,
                balance: a
                    .balance
                    .as_deref()
                    .map_or(Ok(U256::ZERO), |b| quantity("balance", b))?,
                code: a.code.unwrap_or_default(),
                storage: a.storage,
            };
            alloc.insert(address(&addr)?, account);
        }
        Ok(Self {
            nonce: quantity_u64("nonce", &raw.nonce)?,
            timestamp: quantity_u64("timestamp", &raw.timestamp)?,
            extra_data: raw.extra_data,
            gas_limit: quantity_u64("gasLimit", &raw.gas_limit)?,
            difficulty: quantity("difficulty", &raw.difficulty)?,
            mix_hash: raw.mix_hash,
            coinbase: address(&raw.coinbase)?,
            number: quantity_u64("number", &raw.number)?,
            gas_used: quantity_u64("gasUsed", &raw.gas_used)?,
            parent_hash: raw.parent_hash,
            base_fee: raw
                .base_fee_per_gas
                .as_deref()
                .map(|b| quantity("baseFeePerGas", b))
                .transpose()?,
            alloc,
        })
    }

    /// Secure-MPT root over the alloc. Empty accounts are kept, as geth does
    /// for genesis.
    pub fn state_root(&self) -> B256 {
        state_root_unhashed(self.alloc.iter().map(|(addr, a)| {
            let storage = a
                .storage
                .iter()
                .map(|(k, v)| (*k, U256::from_be_bytes(v.0)))
                .filter(|(_, v)| !v.is_zero());
            let account = TrieAccount {
                nonce: a.nonce,
                balance: a.balance,
                storage_root: if a.storage.is_empty() {
                    EMPTY_ROOT_HASH
                } else {
                    storage_root_unhashed(storage)
                },
                code_hash: if a.code.is_empty() {
                    KECCAK256_EMPTY
                } else {
                    keccak256(&a.code)
                },
            };
            (*addr, account)
        }))
    }

    /// Genesis header for `spec`, following geth `toBlockWithRoot`.
    pub fn to_header(&self, spec: &ChainSpec) -> Result<Header, Error> {
        // These add header fields (requests hash, slot number, BAL hash) that
        // are not implemented; refuse rather than emit a wrong hash.
        if spec.prague.is_active(self.number) {
            return Err(Error::Unsupported("prague"));
        }
        if spec.amsterdam.is_active(self.number) {
            return Err(Error::Unsupported("amsterdam"));
        }
        // XDC gates the genesis base fee on the EIP-1559 block, not London.
        let base_fee = if spec.eip1559.is_active(self.number) {
            Some(self.base_fee.unwrap_or(U256::from(INITIAL_BASE_FEE)))
        } else {
            self.base_fee
        };
        Ok(Header {
            parent_hash: self.parent_hash,
            uncle_hash: EMPTY_UNCLE_HASH,
            coinbase: self.coinbase,
            state_root: self.state_root(),
            tx_root: EMPTY_ROOT_HASH,
            receipt_root: EMPTY_ROOT_HASH,
            difficulty: self.difficulty,
            number: self.number,
            gas_limit: if self.gas_limit == 0 {
                GENESIS_GAS_LIMIT
            } else {
                self.gas_limit
            },
            gas_used: self.gas_used,
            timestamp: self.timestamp,
            extra_data: self.extra_data.clone(),
            mix_digest: self.mix_hash,
            nonce: BlockNonce::from_u64(self.nonce),
            base_fee,
            ..Default::default()
        })
    }

    pub fn hash(&self, spec: &ChainSpec) -> Result<B256, Error> {
        Ok(self.to_header(spec)?.hash())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ForkBlock, Network};
    use alloy_primitives::b256;

    fn hash_of(n: Network) -> B256 {
        n.genesis().unwrap().hash(&n.spec()).unwrap()
    }

    // Constants from geth-XDC params/config.go lines 40-44 at the pinned SHA.
    #[test]
    fn genesis_hash_mainnet() {
        assert_eq!(
            hash_of(Network::Mainnet),
            b256!("4a9d748bd78a8d0385b67788c2435dcdb914f98a96250b68863a1f8b7642d6b1")
        );
    }

    #[test]
    fn genesis_hash_apothem() {
        assert_eq!(
            hash_of(Network::Apothem),
            b256!("bdea512b4f12ff1135ec92c00dc047ffb93890c2ea1aa0eefe9b013d80640075")
        );
    }

    #[test]
    fn genesis_hash_devnet() {
        assert_eq!(
            hash_of(Network::Devnet),
            b256!("b8be003946a9c2688e9f1e255a5567c3d144293ccde2ffb38452a5840081b402")
        );
    }

    #[test]
    fn genesis_hash_devnet551() {
        assert_eq!(
            hash_of(Network::Devnet551),
            b256!("50e63112ebf55bba9115ea875eedc3f6c62b42440d23786323e2678a9848b7ac")
        );
    }

    #[test]
    fn base_fee_follows_eip1559_block_not_london() {
        // Devnet has London at 0 but EIP-1559 at 25000: no base fee at genesis.
        let spec = Network::Devnet.spec();
        assert!(spec.london.is_active(0));
        let g = Network::Devnet.genesis().unwrap();
        assert_eq!(g.to_header(&spec).unwrap().base_fee, None);

        let mut early = spec;
        early.eip1559 = ForkBlock::At(0);
        let h = g.to_header(&early).unwrap();
        assert_eq!(h.base_fee, Some(U256::from(INITIAL_BASE_FEE)));
        assert_ne!(h.hash(), hash_of(Network::Devnet));
    }

    #[test]
    fn unsupported_genesis_forks_are_refused() {
        let g = Network::Mainnet.genesis().unwrap();
        let mut spec = Network::Mainnet.spec();
        spec.prague = ForkBlock::At(0);
        assert!(matches!(
            g.to_header(&spec),
            Err(Error::Unsupported("prague"))
        ));
    }

    #[test]
    fn address_prefixes_and_quantities() {
        let a = "00000000000000000000000000000000000000ab";
        let want = Address::from_str(a).unwrap();
        assert_eq!(address(a).unwrap(), want);
        assert_eq!(address(&format!("0x{a}")).unwrap(), want);
        assert_eq!(address(&format!("xdc{a}")).unwrap(), want);
        assert!(address("xdc12").is_err());
        assert_eq!(quantity_u64("n", "0x10").unwrap(), 16);
        assert_eq!(quantity_u64("n", "16").unwrap(), 16);
        assert!(quantity_u64("n", "0xzz").is_err());
    }

    #[test]
    fn pin_file_matches_the_documented_sha() {
        assert_eq!(
            include_str!("../genesis/PIN").trim(),
            "6fc8e8c2088b7f18264f5663036669d04fa7becf"
        );
    }
}
