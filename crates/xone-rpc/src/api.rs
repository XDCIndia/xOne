//! What the handlers need from the node. The node wires these over `xone-sync::Chain` (the
//! [`ChainApi`]), the executor and state of a block ([`StateApi`]), the pool of `xone-builder`
//! ([`PoolApi`]), the peer set ([`NetApi`]) and the XDPoS engine ([`ConsensusApi`]). Every method
//! is synchronous: the server runs handlers on a blocking pool with a deadline (ADR-007 section 4).
//! A backend error is an [`RpcError`]; a read below the retained history is
//! [`xone_sync::bootstrap::retention::Missing`] converted with `?`.

use crate::types::RpcError;
use alloy_primitives::{Address, Bytes, B256, U256};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicBool, Arc},
};
use xone_chainspec::ChainSpec;
use xone_consensus::v1::snapshot::Snapshot;
use xone_primitives::Header;
use xone_state::StateRead;
use xone_sync::bootstrap::retention::{Class, Missing};

impl dyn StateApi {
    /// [`StateApi::state`] returning the value `f` computes.
    pub fn query<R>(&self, block: B256, f: impl FnOnce(&dyn StateRead) -> R) -> Result<R, Missing> {
        let (mut f, mut out) = (Some(f), None);
        self.state(block, &mut |s| out = f.take().map(|f| f(s)))?;
        // an implementation that never ran `f` answers nothing rather than a default
        out.ok_or(Missing::UnsupportedHistory {
            class: Class::State,
            block: 0,
            floor: 0,
            pivot: 0,
        })
    }
}

/// A canonical or side block with its consensus encoding.
#[derive(Debug, Clone)]
pub struct BlockView {
    pub hash: B256,
    pub header: Header,
    /// Transactions as the body carries them (legacy RLP list, or type byte and payload).
    pub txs: Vec<Bytes>,
    pub uncles: Vec<Header>,
    /// The block RLP. `debug_getRawBlock` returns it and `size` is its length (geth `Block.Size`).
    pub raw: Bytes,
}

pub trait ChainApi: Send + Sync {
    fn spec(&self) -> &ChainSpec;
    fn head(&self) -> (u64, B256);
    fn hash_by_number(&self, number: u64) -> Option<B256>;
    fn block(&self, hash: B256) -> Option<BlockView>;
    fn header(&self, hash: B256) -> Option<Header>;
    /// `Receipt.MarshalBinary` of every receipt of the block.
    fn receipts(&self, hash: B256) -> Option<Vec<Bytes>>;
    /// The canonical block and index of a transaction.
    fn tx_location(&self, tx: B256) -> Option<(B256, u64)>;
    /// `Ok` when `class` of `block` is held (XB-14 `Retention::is_within`).
    fn history(&self, class: Class, block: u64) -> Result<(), Missing>;
    /// `(start, current, highest)` while syncing, `None` when in sync.
    fn syncing(&self) -> Option<(u64, u64, u64)>;
    /// The validator key of this node, when it mines (`eth_coinbase`, `eth_mining`; xOneGo 8733eb54).
    fn mining_as(&self) -> Option<Address>;
    /// The number of the highest QC-committed V2 block (the `finalized` tag), `None` before V2.
    fn finalized(&self) -> Option<u64>;
}

/// `eth_call` and `eth_estimateGas` input (geth `TransactionArgs`).
#[derive(Debug, Clone, Default)]
pub struct CallRequest {
    pub from: Option<Address>,
    pub to: Option<Address>,
    pub gas: Option<u64>,
    pub gas_price: Option<U256>,
    pub max_fee: Option<U256>,
    pub max_priority_fee: Option<U256>,
    pub value: Option<U256>,
    pub data: Bytes,
}

#[derive(Debug, Clone)]
pub struct CallOutput {
    pub output: Bytes,
    pub gas_used: u64,
    /// The revert or VM error text, when the call failed.
    pub error: Option<String>,
}

/// A tracer and its config (geth `tracers/native`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tracer {
    Call { only_top_call: bool, with_log: bool },
    Prestate { diff_mode: bool },
}

/// One frame of the `callTracer` output (geth `tracers/native/call.go`).
#[derive(Debug, Clone, Default)]
pub struct CallFrame {
    pub kind: String,
    pub from: Address,
    pub to: Option<Address>,
    pub value: Option<U256>,
    pub gas: u64,
    pub gas_used: u64,
    pub input: Bytes,
    pub output: Option<Bytes>,
    pub error: Option<String>,
    pub revert_reason: Option<String>,
    pub calls: Vec<CallFrame>,
    /// The logs this frame emitted itself, in emission order (`withLog`). A reverted frame and its
    /// reverted descendants carry none, as in geth `callTracer.OnExit` / `clearFailedLogs`.
    pub logs: Vec<CallLog>,
}

/// One `callTracer` log (geth `tracers/native/call.go` `callLog`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallLog {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
    /// The number of child calls made by the frame before the log was emitted.
    pub position: u64,
}

/// One account of the `prestateTracer` output (geth `tracers/native/prestate.go`).
#[derive(Debug, Clone, Default)]
pub struct PreAccount {
    pub balance: Option<U256>,
    pub nonce: Option<u64>,
    pub code: Option<Bytes>,
    pub storage: BTreeMap<B256, B256>,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum TraceOut {
    Call(CallFrame),
    Prestate {
        pre: BTreeMap<Address, PreAccount>,
        /// `diffMode` only.
        post: Option<BTreeMap<Address, PreAccount>>,
    },
}

/// State at a block, and what needs the executor.
pub trait StateApi: Send + Sync {
    /// Runs `f` on the post-state of `block`, pinned for the whole call; `Missing` below the
    /// retained window. A caller reads through `f` so a retention move cannot turn into a default.
    fn state(&self, block: B256, f: &mut dyn FnMut(&dyn StateRead)) -> Result<(), Missing>;
    fn code(&self, code_hash: B256) -> Option<Bytes>;
    /// Runs `req` on the post-state of `block` with the header of `block` as environment.
    fn call(
        &self,
        block: B256,
        req: &CallRequest,
        cancel: &AtomicBool,
    ) -> Result<CallOutput, RpcError>;
    fn estimate_gas(
        &self,
        block: B256,
        req: &CallRequest,
        cancel: &AtomicBool,
    ) -> Result<u64, RpcError>;
    /// Re-executes transaction `index` of `block` on the state of its parent (every transaction
    /// when `index` is `None`) and returns one trace per transaction.
    fn trace(
        &self,
        block: B256,
        index: Option<usize>,
        tracer: &Tracer,
        cancel: &AtomicBool,
    ) -> Result<Vec<TraceOut>, RpcError>;
}

/// The pool, as `txpool_*` and `eth_sendRawTransaction` read it.
pub trait PoolApi: Send + Sync {
    /// `(pending, queued)` counts.
    fn status(&self) -> (usize, usize);
    /// `(pending, queued)` by sender, each sorted by nonce, as consensus-encoded transactions.
    #[allow(clippy::type_complexity)]
    fn content(&self) -> (BTreeMap<Address, Vec<Bytes>>, BTreeMap<Address, Vec<Bytes>>);
    /// Admits a transaction; the message of a refusal is geth's.
    fn add_raw(&self, raw: &[u8]) -> Result<B256, String>;
    /// Next nonce with the executable pool transactions applied (`eth_getTransactionCount` `pending`).
    fn pending_nonce(&self, who: Address) -> Option<u64>;
    fn gas_price(&self) -> U256;
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub id: String,
    pub name: String,
    pub enode: String,
    pub remote: String,
    pub inbound: bool,
    pub trusted: bool,
    pub caps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: String,
    pub name: String,
    pub enode: String,
    pub ip: String,
    pub listen_addr: String,
    pub network: u64,
    pub genesis: B256,
    pub head: B256,
}

pub trait NetApi: Send + Sync {
    /// The one stamp function of the build (`xone --version`, the P2P hello and this are equal).
    fn client_version(&self) -> String;
    fn network_id(&self) -> u64;
    fn peer_count(&self) -> usize;
    fn listening(&self) -> bool;
    fn node_info(&self) -> NodeInfo;
    fn peers(&self) -> Vec<PeerInfo>;
    fn add_peer(&self, enode: &str, trusted: bool) -> Result<bool, RpcError>;
    fn remove_peer(&self, enode: &str, trusted: bool) -> Result<bool, RpcError>;
}

/// The reward report that `XDPoS_getMissedRoundsInEpochByBlockNum` returns.
#[derive(Debug, Clone, Default)]
pub struct MissedRounds {
    pub epoch_round: u64,
    pub epoch_block_number: u64,
    pub missed: Vec<MissedRound>,
}

#[derive(Debug, Clone)]
pub struct MissedRound {
    pub round: u64,
    pub miner: Address,
    pub current_hash: B256,
    pub current_number: u64,
    pub parent_hash: B256,
    pub parent_number: u64,
}

/// An epoch switch: `getBlockInfoByEpochNum`.
#[derive(Debug, Clone)]
pub struct EpochInfo {
    pub hash: B256,
    pub round: u64,
    pub first_block: u64,
    pub last_block: Option<u64>,
}

/// The XDPoS engine reads. Every method is keyed by a header the chain holds.
pub trait ConsensusApi: Send + Sync {
    /// `API.GetSnapshot`: the V1 signer snapshot; V2 headers are the engine's error.
    fn snapshot(&self, hash: B256) -> Result<Snapshot, RpcError>;
    /// `XDPoS.GetMasternodes(chain, header)`.
    fn masternodes(&self, hash: B256) -> Result<Vec<Address>, RpcError>;
    /// `EngineV2.GetRoundNumber(header)`.
    fn round(&self, hash: B256) -> Result<u64, RpcError>;
    /// `EngineV2.GetPreviousPenaltyByHash(hash, 0)`.
    fn penalties(&self, hash: B256) -> Result<Vec<Address>, RpcError>;
    /// The standby pool of the epoch switch block of this block's epoch (`GetStandbynodes`).
    fn standbynodes(&self, hash: B256) -> Result<Vec<Address>, RpcError>;
    /// `EngineV2.GetBlockByEpochNumber`.
    fn epoch_info(&self, epoch: u64) -> Result<EpochInfo, RpcError>;
    /// `EngineV2.GetEpochSwitchInfoBetween`: the numbers of the epoch switch blocks.
    fn epoch_switches_between(&self, begin: B256, end: B256) -> Result<Vec<u64>, RpcError>;
    /// `EngineV2.CalculateMissingRounds`.
    fn missed_rounds(&self, hash: B256) -> Result<MissedRounds, RpcError>;
    /// `getRewardByAccount` reads reward files of `--store-reward`; xOne does not write them.
    fn reward_by_account(
        &self,
        _who: Address,
        _begin: u64,
        _end: u64,
    ) -> Result<serde_json::Value, RpcError> {
        Err(RpcError::server(
            "reward folder not configured; start node with --store-reward to enable reward history",
        ))
    }
}

/// Everything a handler may read.
#[derive(Clone)]
pub struct Backend {
    pub chain: Arc<dyn ChainApi>,
    pub state: Arc<dyn StateApi>,
    pub pool: Arc<dyn PoolApi>,
    pub net: Arc<dyn NetApi>,
    pub consensus: Arc<dyn ConsensusApi>,
}
