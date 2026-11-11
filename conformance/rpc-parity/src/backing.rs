//! The chain the sweep runs on: the blocks the harness holds (Apothem 0..=1800, committed in
//! `conformance/vectors/replay/`) executed by `xone-replay`, with the receipts and state of every
//! block it produced. Anything beyond that is reported `not-backed`, with the reason.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::{atomic::AtomicBool, Arc},
};
use xone_chainspec::{ChainSpec, Network};
use xone_consensus::v1::{checkpoint::masternodes_of, snapshot::Snapshot};
use xone_harness::{
    cache,
    replay::{self, Opts},
};
use xone_primitives::Header;
use xone_rpc::{api::*, types::RpcError};
use xone_state::{FlatRead, StateRead};
use xone_sync::bootstrap::retention::{Class, Missing};

pub struct Backing {
    pub tip: u64,
    pub net: String,
    d: Option<Arc<Data>>,
}

struct Data {
    spec: ChainSpec,
    headers: Vec<Header>,
    hashes: Vec<B256>,
    txs: Vec<Vec<Bytes>>,
    uncles: Vec<Vec<Header>>,
    raw: Vec<Bytes>,
    receipts: Vec<Vec<Bytes>>,
    states: Vec<xone_state::MemFlat>,
    codes: BTreeMap<B256, Bytes>,
    snaps: Vec<Snapshot>,
    by_hash: HashMap<B256, usize>,
    tx_loc: HashMap<B256, (B256, u64)>,
}

fn not_backed(why: &str) -> RpcError {
    RpcError::server(format!("not backed: {why}"))
}

impl Backing {
    pub fn load(net: &str, tip: u64) -> Result<Self, String> {
        if net != "apothem" {
            return Ok(Self {
                tip: 0,
                net: net.to_owned(),
                d: None,
            });
        }
        let p =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../vectors/replay/apothem-0-1800.jsonl.gz");
        let lines = cache::read(&p)?;
        let take = usize::try_from(tip).map_err(|e| e.to_string())? + 1;
        let lines: Vec<_> = lines.into_iter().take(take).collect();
        let blocks: Vec<_> = lines.iter().map(cache::decode).collect::<Result<_, _>>()?;
        let spec = Network::Apothem.spec();
        let opts = Opts {
            to: tip,
            state_dir: None,
            every: 0,
            resume: false,
            keep: true,
            progress: 0,
            gap_offset: 0,
            dump_state: None,
        };
        let rep = replay::run(Network::Apothem, &spec, &blocks, &opts)?;
        if let Some(d) = &rep.divergence {
            return Err(format!(
                "the replay diverged at block {}: the backing chain is not trusted",
                d.block
            ));
        }
        let (mut flat, mut codes, g) = replay::genesis(Network::Apothem, &spec)?;
        let mut states = vec![flat.clone()];
        let mut receipts = vec![vec![]];
        let g_masternodes = masternodes_of(&g.extra_data).map_err(|e| e.to_string())?;
        let mut snaps = vec![Snapshot::new(0, g.hash(), g_masternodes)];
        for x in &rep.executed {
            x.diff.apply(&mut flat).map_err(|e| e.to_string())?;
            states.push(flat.clone());
            receipts.push(x.receipts.clone());
            snaps.push(x.snapshot.clone());
            codes.extend(x.codes.clone());
        }
        let hashes: Vec<B256> = blocks.iter().map(|b| b.header.hash()).collect();
        let mut tx_loc = HashMap::new();
        for (i, b) in blocks.iter().enumerate() {
            for (j, t) in b.body.transactions.iter().enumerate() {
                tx_loc.insert(
                    keccak256(t),
                    (hashes[i], u64::try_from(j).map_err(|e| e.to_string())?),
                );
            }
        }
        let d = Data {
            by_hash: hashes.iter().enumerate().map(|(i, h)| (*h, i)).collect(),
            headers: blocks.iter().map(|b| b.header.clone()).collect(),
            hashes,
            txs: blocks.iter().map(|b| b.body.transactions.clone()).collect(),
            uncles: blocks.iter().map(|b| b.body.uncles.clone()).collect(),
            raw: lines.iter().map(|l| l.raw.clone()).collect(),
            receipts,
            states,
            codes,
            snaps,
            tx_loc,
            spec,
        };
        Ok(Self {
            tip: rep.last_ok,
            net: net.to_owned(),
            d: Some(Arc::new(d)),
        })
    }

    pub fn network_unbacked(&self) -> Option<String> {
        self.d.is_none().then(|| format!("{} has no chain spec in xone-chainspec and no replayed blocks (only Apothem 0..=1800 is in the harness)", self.net))
    }

    /// Why this recorded request cannot be replayed here, `None` when the data exists.
    pub fn unbacked(&self, line: &Value) -> Option<String> {
        // `getEpochNumbersBetween(begin, end)`: both ends must be known.
        let end = line["request"]["params"][1]
            .as_str()
            .and_then(|e| u64::from_str_radix(e.strip_prefix("0x")?, 16).ok());
        if let (Some(e), "XDPoS_getEpochNumbersBetween") =
            (end, line["request"]["method"].as_str()?)
        {
            if e > self.tip {
                return Some(format!(
                    "end block {e} is above the replayed tip {}",
                    self.tip
                ));
            }
        }
        let n = line["block"].as_u64()?;
        (n > self.tip).then(|| {
            format!(
                "block {n} is above the replayed tip {}: no executed block, receipts or state",
                self.tip
            )
        })
    }

    pub fn backend(&self) -> Backend {
        let d = self.d.clone().unwrap_or_else(|| Arc::new(empty()));
        Backend {
            chain: Arc::new(C(d.clone())),
            state: Arc::new(S(d.clone())),
            pool: Arc::new(P),
            net: Arc::new(N(d.spec.chain_id)),
            consensus: Arc::new(K(d)),
        }
    }
}

fn empty() -> Data {
    let spec = Network::Apothem.spec();
    let g = Header::default();
    Data {
        headers: vec![g.clone()],
        hashes: vec![g.hash()],
        txs: vec![vec![]],
        uncles: vec![vec![]],
        raw: vec![Bytes::new()],
        receipts: vec![vec![]],
        states: vec![Default::default()],
        codes: BTreeMap::new(),
        snaps: vec![Snapshot::new(0, g.hash(), [])],
        by_hash: HashMap::new(),
        tx_loc: HashMap::new(),
        spec,
    }
}

struct C(Arc<Data>);
struct S(Arc<Data>);
struct P;
struct N(u64);
struct K(Arc<Data>);

impl ChainApi for C {
    fn spec(&self) -> &ChainSpec {
        &self.0.spec
    }
    fn head(&self) -> (u64, B256) {
        let n = self.0.headers.len() - 1;
        (u64::try_from(n).unwrap_or(0), self.0.hashes[n])
    }
    fn hash_by_number(&self, n: u64) -> Option<B256> {
        self.0.hashes.get(usize::try_from(n).ok()?).copied()
    }
    fn block(&self, hash: B256) -> Option<BlockView> {
        let i = *self.0.by_hash.get(&hash)?;
        Some(BlockView {
            hash,
            header: self.0.headers[i].clone(),
            txs: self.0.txs[i].clone(),
            uncles: self.0.uncles[i].clone(),
            raw: self.0.raw[i].clone(),
        })
    }
    fn header(&self, hash: B256) -> Option<Header> {
        self.0
            .by_hash
            .get(&hash)
            .map(|i| self.0.headers[*i].clone())
    }
    fn receipts(&self, hash: B256) -> Option<Vec<Bytes>> {
        self.0
            .by_hash
            .get(&hash)
            .map(|i| self.0.receipts[*i].clone())
    }
    fn tx_location(&self, tx: B256) -> Option<(B256, u64)> {
        self.0.tx_loc.get(&tx).copied()
    }
    fn history(&self, _: Class, _: u64) -> Result<(), Missing> {
        Ok(())
    }
    fn syncing(&self) -> Option<(u64, u64, u64)> {
        None
    }
    fn mining_as(&self) -> Option<Address> {
        None
    }
    fn finalized(&self) -> Option<u64> {
        None
    }
}

struct View<'a>(&'a xone_state::MemFlat);
impl StateRead for View<'_> {
    fn account(&self, a: Address) -> Option<xone_state::Account> {
        FlatRead::account(self.0, a)
    }
    fn storage(&self, a: Address, k: B256) -> U256 {
        FlatRead::storage(self.0, a, k)
    }
}

impl StateApi for S {
    fn state(&self, block: B256) -> Result<Box<dyn StateRead + '_>, Missing> {
        let i = self
            .0
            .by_hash
            .get(&block)
            .ok_or(Missing::UnsupportedHistory {
                class: Class::State,
                block: 0,
                floor: 0,
                pivot: 0,
            })?;
        Ok(Box::new(View(&self.0.states[*i])))
    }
    fn code(&self, h: B256) -> Option<Bytes> {
        self.0.codes.get(&h).cloned()
    }
    fn call(&self, _: B256, _: &CallRequest, _: &AtomicBool) -> Result<CallOutput, RpcError> {
        Err(not_backed(
            "eth_call needs the executor bound to StateApi (node wiring)",
        ))
    }
    fn estimate_gas(&self, _: B256, _: &CallRequest, _: &AtomicBool) -> Result<u64, RpcError> {
        Err(not_backed(
            "eth_estimateGas needs the executor bound to StateApi (node wiring)",
        ))
    }
    fn trace(
        &self,
        _: B256,
        _: Option<usize>,
        _: &Tracer,
        _: &AtomicBool,
    ) -> Result<Vec<TraceOut>, RpcError> {
        Err(not_backed("debug_trace* needs the executor with a tracing inspector bound to StateApi (node wiring)"))
    }
}

impl PoolApi for P {
    fn status(&self) -> (usize, usize) {
        (0, 0)
    }
    fn content(&self) -> (BTreeMap<Address, Vec<Bytes>>, BTreeMap<Address, Vec<Bytes>>) {
        Default::default()
    }
    fn add_raw(&self, _: &[u8]) -> Result<B256, String> {
        Err("not backed: no pool in the sweep".into())
    }
    fn pending_nonce(&self, _: Address) -> Option<u64> {
        None
    }
    fn gas_price(&self) -> U256 {
        U256::ZERO
    }
}

impl NetApi for N {
    fn client_version(&self) -> String {
        "xone-rpc-parity".into()
    }
    fn network_id(&self) -> u64 {
        self.0
    }
    fn peer_count(&self) -> usize {
        0
    }
    fn listening(&self) -> bool {
        false
    }
    fn node_info(&self) -> NodeInfo {
        NodeInfo {
            id: String::new(),
            name: String::new(),
            enode: String::new(),
            ip: String::new(),
            listen_addr: String::new(),
            network: self.0,
            genesis: B256::ZERO,
            head: B256::ZERO,
        }
    }
    fn peers(&self) -> Vec<PeerInfo> {
        vec![]
    }
    fn add_peer(&self, _: &str, _: bool) -> Result<bool, RpcError> {
        Ok(false)
    }
    fn remove_peer(&self, _: &str, _: bool) -> Result<bool, RpcError> {
        Ok(false)
    }
}

impl K {
    fn index(&self, h: B256) -> Result<usize, RpcError> {
        self.0
            .by_hash
            .get(&h)
            .copied()
            .ok_or_else(|| RpcError::server("unknown block"))
    }
}

impl ConsensusApi for K {
    fn snapshot(&self, hash: B256) -> Result<Snapshot, RpcError> {
        Ok(self.0.snaps[self.index(hash)?].clone())
    }
    /// V1: the list in the `extraData` of the checkpoint of the block's epoch.
    fn masternodes(&self, hash: B256) -> Result<Vec<Address>, RpcError> {
        let i = self.index(hash)?;
        let e = usize::try_from(self.0.spec.xdpos.epoch)
            .map_err(|e| RpcError::server(e.to_string()))?;
        masternodes_of(&self.0.headers[i - i % e].extra_data)
            .map_err(|e| RpcError::server(e.to_string()))
    }
    fn round(&self, _: B256) -> Result<u64, RpcError> {
        Err(not_backed("V2 round: no V2 block is replayed"))
    }
    fn penalties(&self, _: B256) -> Result<Vec<Address>, RpcError> {
        Err(not_backed("V2 penalties: no V2 block is replayed"))
    }
    fn standbynodes(&self, _: B256) -> Result<Vec<Address>, RpcError> {
        Err(not_backed("V2 standby pool: no V2 block is replayed"))
    }
    fn epoch_info(&self, _: u64) -> Result<EpochInfo, RpcError> {
        Err(not_backed("V2 epoch info: no V2 block is replayed"))
    }
    fn epoch_switches_between(&self, _: B256, _: B256) -> Result<Vec<u64>, RpcError> {
        Err(not_backed("V2 epoch switches: no V2 block is replayed"))
    }
    fn missed_rounds(&self, _: B256) -> Result<MissedRounds, RpcError> {
        Err(not_backed("V2 missed rounds: no V2 block is replayed"))
    }
}
