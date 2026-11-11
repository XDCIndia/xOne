//! The line format of `conformance/vectors/consensus/{valid,negative}/*.jsonl`
//! (documented in that directory's README). Included by the generator and by
//! `crates/xone-consensus/tests/corpus.rs`, so both read the same shape.

use alloy_primitives::{Address, Bytes, B256};
use serde::{Deserialize, Serialize};
use xone_chainspec::{ChainSpec, Network};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vector {
    pub id: String,
    pub network: String,
    pub class: String,
    pub number: u64,
    pub hash: B256,
    /// `rlp(header)`.
    pub header: Bytes,
    pub parent: Bytes,
    pub body: VBody,
    /// `Receipt.MarshalBinary` of each receipt.
    pub receipts: Vec<Bytes>,
    pub context: VContext,
    pub oracle: Oracle,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Present on negative vectors only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation: Option<Mutation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VBody {
    pub transactions: Vec<Bytes>,
    /// `rlp(header)` of each uncle.
    pub uncles: Vec<Bytes>,
    pub withdrawals: Option<Vec<Bytes>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VContext {
    pub consensus: String,
    pub now: u64,
    pub masternodes: Vec<Address>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_masternodes: Vec<Address>,
    pub penalties: Vec<Address>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_epoch_switch: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signers: Vec<Address>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<Bytes>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkpoint_signers: Vec<Address>,
}

/// What pinned geth-XDC computed for the header (see `conformance/exporter/headers.go`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Oracle {
    pub consensus: String,
    pub seal_hash: B256,
    pub v2_sig_hash: B256,
    pub hash_no_validator: B256,
    pub signer: Option<Address>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mutation {
    pub rule: String,
    /// `header` runs the header verifier, `body` the body verifier.
    pub stage: String,
    pub recipe: String,
    /// The variant name of the typed error the verifier must return.
    pub expect: String,
    /// `id` of the valid vector the mutation started from.
    pub source: String,
    /// Forks that are not scheduled on the vector's network and are switched on from block 0
    /// for this vector, so the rule behind them has a vector at all. Names: `validator_order`,
    /// `signing_tx_bound`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activate: Vec<String>,
}

/// The chain spec a vector is verified under: its network's, with the forks the mutation switches on.
pub fn spec_of(v: &Vector) -> ChainSpec {
    let mut spec = spec(&v.network);
    for name in v.mutation.iter().flat_map(|m| &m.activate) {
        match name.as_str() {
            "validator_order" => spec.validator_order = xone_chainspec::ForkBlock::At(0),
            "signing_tx_bound" => spec.signing_tx_bound = xone_chainspec::ForkBlock::At(0),
            other => panic!("unknown fork {other}"),
        }
    }
    spec
}

pub fn vectors_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/vectors/consensus")
}

pub fn load_file(path: &std::path::Path) -> Vec<Vector> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .collect()
}

/// The chain spec of a vector's network: Apothem is built in, net2040 comes from the
/// fork table the exporter wrote next to the vectors.
pub fn spec(network: &str) -> ChainSpec {
    match network {
        "apothem" => Network::Apothem.spec(),
        "net2040" => {
            let json = std::fs::read_to_string(vectors_dir().join("net2040.forks.json")).unwrap();
            xone_chainspec::load_named("net2040", &json).unwrap()
        }
        other => panic!("no chain spec for network {other}"),
    }
}
