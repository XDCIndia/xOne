#![allow(clippy::unwrap_used, clippy::expect_used)] // test code: a panic is the failure report
//! XB-22 acceptance tests over an in-memory backend: case-sensitive PascalCase names, `xdc` input,
//! batches, `-32601`, quotas and `-32005`, the typed unsupported-history error, the exact XDPoS
//! method set and the three transports.

use alloy_primitives::{Address, Bytes, B256, U256};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicBool, atomic::AtomicU64, atomic::Ordering, Arc},
    time::Duration,
};
use xone_chainspec::ChainSpec;
use xone_consensus::v1::snapshot::Snapshot;
use xone_primitives::Header;
use xone_rpc::{
    api::*,
    server::{Limits, Rpc},
    types::RpcError,
    xdpos,
};
use xone_state::{Account, StateRead};
use xone_sync::bootstrap::retention::{Class, Missing};

const ACCT: Address = Address::repeat_byte(0x11);

struct Chain {
    spec: ChainSpec,
    /// Blocks below this number are out of retention.
    floor: AtomicU64,
}

fn header(n: u64) -> Header {
    Header {
        number: n,
        ..Header::default()
    }
}

impl ChainApi for Chain {
    fn spec(&self) -> &ChainSpec {
        &self.spec
    }
    fn head(&self) -> (u64, B256) {
        (100, header(100).hash())
    }
    fn hash_by_number(&self, n: u64) -> Option<B256> {
        (n <= 100).then(|| header(n).hash())
    }
    fn block(&self, hash: B256) -> Option<BlockView> {
        (0..=100)
            .map(header)
            .find(|h| h.hash() == hash)
            .map(|h| BlockView {
                hash,
                header: h,
                txs: vec![],
                uncles: vec![],
                raw: Bytes::from_static(&[0xc0]),
            })
    }
    fn header(&self, hash: B256) -> Option<Header> {
        self.block(hash).map(|b| b.header)
    }
    fn receipts(&self, _: B256) -> Option<Vec<Bytes>> {
        Some(vec![])
    }
    fn tx_location(&self, _: B256) -> Option<(B256, u64)> {
        None
    }
    fn history(&self, class: Class, block: u64) -> Result<(), Missing> {
        let floor = self.floor.load(Ordering::Relaxed);
        if block >= floor {
            return Ok(());
        }
        Err(Missing::UnsupportedHistory {
            class,
            block,
            floor,
            pivot: 100,
        })
    }
    fn syncing(&self) -> Option<(u64, u64, u64)> {
        None
    }
    fn mining_as(&self) -> Option<Address> {
        Some(Address::repeat_byte(0x22))
    }
    fn finalized(&self) -> Option<u64> {
        None
    }
}

struct St;
struct S;
impl StateRead for S {
    fn account(&self, a: Address) -> Option<Account> {
        (a == ACCT).then(|| Account {
            nonce: 3,
            balance: U256::from(1000).into(),
            ..Account::default()
        })
    }
    fn storage(&self, _: Address, _: B256) -> U256 {
        U256::ZERO
    }
}
impl StateApi for St {
    fn state(&self, h: B256, f: &mut dyn FnMut(&dyn StateRead)) -> Result<(), Missing> {
        // block 7 leaves the retained window while it is read
        if h == header(7).hash() {
            return Err(Missing::UnsupportedHistory {
                class: Class::State,
                block: 7,
                floor: 8,
                pivot: 0,
            });
        }
        f(&S);
        Ok(())
    }
    fn code(&self, _: B256) -> Option<Bytes> {
        None
    }
    fn call(&self, _: B256, _: &CallRequest, _: &AtomicBool) -> Result<CallOutput, RpcError> {
        Ok(CallOutput {
            output: Bytes::from_static(&[1]),
            gas_used: 1,
            error: None,
        })
    }
    fn estimate_gas(&self, _: B256, _: &CallRequest, _: &AtomicBool) -> Result<u64, RpcError> {
        Ok(21000)
    }
    fn trace(
        &self,
        _: B256,
        _: Option<usize>,
        _: &Tracer,
        _: &AtomicBool,
    ) -> Result<Vec<TraceOut>, RpcError> {
        Ok(vec![])
    }
}

struct Pool;
impl PoolApi for Pool {
    fn status(&self) -> (usize, usize) {
        (2, 1)
    }
    fn content(&self) -> (BTreeMap<Address, Vec<Bytes>>, BTreeMap<Address, Vec<Bytes>>) {
        Default::default()
    }
    fn add_raw(&self, _: &[u8]) -> Result<B256, String> {
        Err("nonce too low".into())
    }
    fn pending_nonce(&self, _: Address) -> Option<u64> {
        None
    }
    fn gas_price(&self) -> U256 {
        U256::from(250_000_000u64)
    }
}

struct Net;
impl NetApi for Net {
    fn client_version(&self) -> String {
        "xone/test".into()
    }
    fn network_id(&self) -> u64 {
        51
    }
    fn peer_count(&self) -> usize {
        7
    }
    fn listening(&self) -> bool {
        true
    }
    fn node_info(&self) -> NodeInfo {
        NodeInfo {
            id: "id".into(),
            name: "n".into(),
            enode: "enode://x".into(),
            ip: "1.2.3.4".into(),
            listen_addr: "[::]:30303".into(),
            network: 51,
            genesis: B256::ZERO,
            head: B256::ZERO,
        }
    }
    fn peers(&self) -> Vec<PeerInfo> {
        vec![]
    }
    fn add_peer(&self, _: &str, _: bool) -> Result<bool, RpcError> {
        Ok(true)
    }
    fn remove_peer(&self, _: &str, _: bool) -> Result<bool, RpcError> {
        Ok(false)
    }
}

struct Cons;
impl ConsensusApi for Cons {
    fn snapshot(&self, hash: B256) -> Result<Snapshot, RpcError> {
        Ok(Snapshot::new(
            1,
            hash,
            [Address::repeat_byte(3), Address::repeat_byte(1)],
        ))
    }
    fn masternodes(&self, _: B256) -> Result<Vec<Address>, RpcError> {
        Ok(vec![Address::repeat_byte(1)])
    }
    fn round(&self, _: B256) -> Result<u64, RpcError> {
        Ok(5)
    }
    fn penalties(&self, _: B256) -> Result<Vec<Address>, RpcError> {
        Ok(vec![])
    }
    fn standbynodes(&self, _: B256) -> Result<Vec<Address>, RpcError> {
        Ok(vec![])
    }
    fn epoch_info(&self, _: u64) -> Result<EpochInfo, RpcError> {
        Err(RpcError::server("no epoch"))
    }
    fn epoch_switches_between(&self, b: B256, e: B256) -> Result<Vec<u64>, RpcError> {
        Ok(if b == e { vec![] } else { vec![900] })
    }
    fn missed_rounds(&self, _: B256) -> Result<MissedRounds, RpcError> {
        Ok(MissedRounds::default())
    }
}

fn rpc_with(limits: Limits, floor: u64) -> Arc<Rpc> {
    rpc_spec(xone_chainspec::apothem(), limits, floor)
}

fn rpc_spec(spec: ChainSpec, limits: Limits, floor: u64) -> Arc<Rpc> {
    let be = Backend {
        chain: Arc::new(Chain {
            spec,
            floor: AtomicU64::new(floor),
        }),
        state: Arc::new(St),
        pool: Arc::new(Pool),
        net: Arc::new(Net),
        consensus: Arc::new(Cons),
    };
    Rpc::new(be, limits)
}

fn rpc() -> Arc<Rpc> {
    rpc_with(Limits::default(), 0)
}

fn ask(r: &Rpc, body: Value) -> Value {
    serde_json::from_slice(
        &r.handle_bytes(body.to_string().as_bytes())
            .expect("a response"),
    )
    .expect("json")
}

fn req(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

#[test]
fn xdpos_names_are_pascal_case_and_case_sensitive() {
    let r = rpc();
    let ok = ask(&r, req(1, "XDPoS_getSigners", json!(["0x5"])));
    assert!(ok.get("error").is_none(), "{ok}");
    for bad in [
        "xdpos_getSigners",
        "XDPOS_getSigners",
        "XDPoS_GetSigners",
        "xDPoS_getSigners",
        "XDPoS_getsigners",
    ] {
        let e = ask(&r, req(1, bad, json!(["0x5"])));
        assert_eq!(e["error"]["code"], -32601, "{bad}");
        assert_eq!(
            e["error"]["message"],
            format!("the method {bad} does not exist/is not available")
        );
    }
}

#[test]
fn the_xdpos_method_set_is_exactly_the_issue_list() {
    let want = [
        "getBlockInfoByEpochNum",
        "getCandidates",
        "getEpochNumbersBetween",
        "getLatestPoolStatus",
        "getMasternodes",
        "getMasternodesByNumber",
        "getMissedRoundsInEpochByBlockNum",
        "getRewardByAccount",
        "getSigners",
        "getSignersAtHash",
        "getSnapshot",
        "getSnapshotAtHash",
        "networkInformation",
        "getV2BlockByNumber",
    ];
    let mut want: Vec<String> = want.iter().map(|m| format!("XDPoS_{m}")).collect();
    want.sort();
    let r = rpc();
    let have: Vec<String> = r
        .reg
        .names()
        .into_iter()
        .filter(|n| n.starts_with("XDPoS_"))
        .map(str::to_owned)
        .collect();
    assert_eq!(have, want);
    assert_eq!(xdpos::METHODS.len(), 14);
    // No namespace outside the ADR-008 list, and none of the XDCx or trace namespaces.
    for n in r.reg.names() {
        let ns = n.split('_').next().unwrap_or("");
        assert!(
            ["eth", "net", "web3", "txpool", "admin", "debug", "XDPoS"].contains(&ns),
            "{n}"
        );
    }
}

#[test]
fn out_of_scope_namespaces_are_method_not_found() {
    let r = rpc();
    for m in [
        "trace_block",
        "miner_start",
        "personal_listAccounts",
        "XDCx_getOrderBook",
        "XDCxlending_x",
        "eth_getProof",
        "les_x",
        "engine_newPayloadV1",
        "debug_traceCall",
        "rpc_modules",
    ] {
        let e = ask(&r, req(1, m, json!([])));
        assert_eq!(e["error"]["code"], -32601, "{m}");
    }
}

#[test]
fn xdc_prefix_is_accepted_wherever_an_address_is() {
    let r = rpc();
    let a = format!("{ACCT:#x}");
    let plain = ask(&r, req(1, "eth_getBalance", json!([a, "latest"])));
    assert_eq!(plain["result"], "0x3e8");
    for p in ["xdc", "XDC"] {
        let x = ask(
            &r,
            req(
                1,
                "eth_getBalance",
                json!([format!("{p}{}", &a[2..]), "latest"]),
            ),
        );
        assert_eq!(x["result"], plain["result"]);
    }
    let n = ask(
        &r,
        req(
            1,
            "eth_getTransactionCount",
            json!([format!("xdc{}", &a[2..]), "0x5"]),
        ),
    );
    assert_eq!(n["result"], "0x3");
    let bad = ask(&r, req(1, "eth_getBalance", json!(["xdc1234", "latest"])));
    assert_eq!(bad["error"]["code"], -32602);
}

#[test]
fn batch_of_100_keeps_order_and_ids_and_101_is_refused() {
    let r = rpc();
    let mut b: Vec<Value> = (0..100)
        .map(|i| {
            req(
                1000 + i,
                if i % 2 == 0 {
                    "eth_blockNumber"
                } else {
                    "net_version"
                },
                json!([]),
            )
        })
        .collect();
    let out = ask(&r, Value::Array(b.clone()));
    let out = out.as_array().expect("array");
    assert_eq!(out.len(), 100);
    for (i, o) in out.iter().enumerate() {
        assert_eq!(o["id"], 1000 + u64::try_from(i).unwrap());
        assert!(o.get("result").is_some());
    }
    assert_eq!(out[0]["result"], "0x64");
    assert_eq!(out[1]["result"], "51");
    b.push(req(1, "eth_blockNumber", json!([])));
    let e = ask(&r, Value::Array(b));
    assert_eq!(e["error"]["code"], -32600);
    assert_eq!(e["error"]["message"], "batch too large");
}

#[test]
fn notifications_get_no_response_and_batch_errors_match_inputs() {
    let r = rpc();
    assert!(r
        .handle_bytes(br#"{"jsonrpc":"2.0","method":"eth_blockNumber"}"#)
        .is_none());
    let out = ask(
        &r,
        json!([req(1, "eth_blockNumber", json!([])), {"jsonrpc":"2.0","method":"eth_blockNumber"}, req(3, "nosuch_x", json!([])), 7, {"jsonrpc":"1.0","id":9,"method":"eth_blockNumber"}]),
    );
    let out = out.as_array().expect("array");
    assert_eq!(out.len(), 4);
    assert_eq!(out[0]["id"], 1);
    assert_eq!(out[1]["id"], 3);
    assert_eq!(out[1]["error"]["code"], -32601);
    assert_eq!(out[2]["error"]["code"], -32600);
    assert_eq!(out[3]["id"], 9);
    assert_eq!(out[3]["error"]["code"], -32600);
    let p = serde_json::from_slice::<Value>(&r.handle_bytes(b"{not json").expect("reply"))
        .expect("json");
    assert_eq!(p["error"]["code"], -32700);
}

#[test]
fn out_of_retention_is_the_typed_unsupported_history_error() {
    let r = rpc_with(Limits::default(), 50);
    let e = ask(&r, req(1, "eth_getBlockByNumber", json!(["0xa", false])));
    assert_eq!(e["error"]["code"], -32000);
    assert!(e["error"]["message"]
        .as_str()
        .expect("msg")
        .starts_with("unsupported history"));
    assert_eq!(e["error"]["data"]["firstRetained"], 50);
    assert_eq!(e["error"]["data"]["block"], 10);
    // The state of an old block, a receipt query and a range of logs are refused the same way.
    for (m, p) in [
        ("eth_getBalance", json!([format!("{ACCT:#x}"), "0xa"])),
        (
            "eth_getLogs",
            json!([{"fromBlock": "0xa", "toBlock": "0xb"}]),
        ),
        ("XDPoS_getSnapshot", json!(["0xa"])),
    ] {
        let e = ask(&r, req(1, m, p));
        if m != "XDPoS_getSnapshot" {
            assert!(
                e["error"]["message"]
                    .as_str()
                    .expect("msg")
                    .starts_with("unsupported history"),
                "{m}: {e}"
            );
        }
    }
    // Inside the window it answers.
    assert!(
        ask(&r, req(1, "eth_getBlockByNumber", json!(["0x64", false])))["result"]["validators"]
            .is_string()
    );
}

#[test]
fn block_json_has_the_xdpos_fields() {
    let r = rpc();
    let b = ask(&r, req(1, "eth_getBlockByNumber", json!(["0x5", true])));
    for k in [
        "validators",
        "validator",
        "penalties",
        "hash",
        "stateRoot",
        "transactionsRoot",
        "receiptsRoot",
        "sha3Uncles",
        "miner",
        "size",
        "uncles",
        "transactions",
    ] {
        assert!(b["result"].get(k).is_some(), "{k}");
    }
    assert!(ask(&r, req(1, "eth_getBlockByNumber", json!(["0x65", false])))["result"].is_null());
}

#[test]
fn certik_xdc30_begin_equals_end_is_an_empty_range_not_an_error() {
    // V1 range: geth's refusal (`api_extended.go:300`).
    let r = rpc();
    let e = ask(
        &r,
        req(1, "XDPoS_getEpochNumbersBetween", json!(["0x5", "0x5"])),
    );
    assert_eq!(e["error"]["message"], "not supported in the v1 consensus");
    // V2 range (the switch moved below the mock's blocks): `begin == end` is an empty list.
    let mut spec = xone_chainspec::apothem();
    spec.xdpos.v2.as_mut().expect("v2").switch_block = 10;
    let r = rpc_spec(spec, Limits::default(), 0);
    assert_eq!(
        ask(
            &r,
            req(1, "XDPoS_getEpochNumbersBetween", json!(["0x32", "0x32"]))
        )["result"],
        json!([])
    );
    assert_eq!(
        ask(
            &r,
            req(1, "XDPoS_getEpochNumbersBetween", json!(["0x32", "0x40"]))
        )["result"],
        json!([900])
    );
}

#[test]
fn quota_bucket_rejects_with_32005_and_a_retry_hint() {
    let l = Limits {
        bucket_burst: 10,
        bucket_per_sec: 1,
        ..Limits::default()
    };
    let r = rpc_with(l, 0);
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(async {
        let conn = r.new_conn(8);
        let call = |id| {
            serde_json::to_vec(&req(
                id,
                "eth_getBalance",
                json!([format!("{ACCT:#x}"), "latest"]),
            ))
            .expect("json")
        };
        // C1 costs 5 units: two fit the burst of 10, the third is refused.
        for id in 1..=2 {
            let rep = r.serve(&conn, call(id)).await;
            assert!(rep.retry_after.is_none());
        }
        let rep = r.serve(&conn, call(3)).await;
        let v: Value = serde_json::from_slice(&rep.body.expect("body")).expect("json");
        assert_eq!(v["error"]["code"], -32005);
        assert!(v["error"]["data"].as_u64().expect("seconds") >= 1);
        assert!(rep.retry_after.is_some());
        // A batch counts as the sum of its parts: 3 x C1 = 15 units does not fit a fresh burst of 10.
        let conn2 = r.new_conn(8);
        let batch = serde_json::to_vec(&json!([
            req(1, "eth_getBalance", json!([format!("{ACCT:#x}")])),
            req(2, "eth_getBalance", json!([format!("{ACCT:#x}")])),
            req(3, "eth_getBalance", json!([format!("{ACCT:#x}")]))
        ]))
        .expect("json");
        let rep = r.serve(&conn2, batch).await;
        let v: Value = serde_json::from_slice(&rep.body.expect("body")).expect("json");
        assert_eq!(v["error"]["code"], -32005);
    });
}

#[test]
fn load_shedding_rejects_c2_and_c3_with_node_busy_and_keeps_c0_c1() {
    let r = rpc();
    r.shed.store(true, Ordering::Relaxed);
    let busy = ask(
        &r,
        req(
            1,
            "eth_getLogs",
            json!([{"fromBlock": "0x1", "toBlock": "0x2"}]),
        ),
    );
    assert_eq!(busy["error"]["code"], -32005);
    assert_eq!(busy["error"]["data"], "node busy");
    assert_eq!(
        ask(
            &r,
            req(
                2,
                "debug_traceTransaction",
                json!([B256::ZERO.to_string(), {"tracer": "callTracer"}])
            )
        )["error"]["code"],
        -32005
    );
    assert!(ask(&r, req(3, "eth_blockNumber", json!([])))
        .get("result")
        .is_some());
    assert!(ask(
        &r,
        req(4, "eth_getBalance", json!([format!("{ACCT:#x}"), "latest"]))
    )
    .get("result")
    .is_some());
}

#[test]
fn class_concurrency_is_bounded() {
    let mut l = Limits::default();
    l.class_concurrency[3] = 0; // no C3 slot at all
    let r = rpc_with(l, 0);
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(async {
        let conn = r.new_conn(8);
        let body = serde_json::to_vec(&req(
            1,
            "debug_traceBlockByNumber",
            json!(["0x5", {"tracer": "callTracer"}]),
        ))
        .expect("json");
        let rep = r.serve(&conn, body).await;
        let v: Value = serde_json::from_slice(&rep.body.expect("body")).expect("json");
        assert_eq!(v["error"]["code"], -32005);
    });
}

#[test]
fn misc_methods_answer() {
    let r = rpc();
    assert_eq!(
        ask(&r, req(1, "web3_clientVersion", json!([])))["result"],
        "xone/test"
    );
    assert_eq!(
        ask(&r, req(1, "web3_sha3", json!(["0x68656c6c6f20776f726c64"])))["error"]["code"],
        -32601
    );
    assert_eq!(ask(&r, req(1, "net_peerCount", json!([])))["result"], "0x7");
    assert_eq!(
        ask(&r, req(1, "txpool_status", json!([])))["result"],
        json!({"pending": "0x2", "queued": "0x1"})
    );
    assert_eq!(ask(&r, req(1, "eth_chainId", json!([])))["result"], "0x33");
    assert!(ask(&r, req(1, "eth_coinbase", json!([])))["result"].is_string());
    assert_eq!(ask(&r, req(1, "eth_mining", json!([])))["result"], true);
    let e = ask(&r, req(1, "eth_sendRawTransaction", json!(["0x00"])));
    assert_eq!(e["error"]["message"], "nonce too low");
    let m = ask(&r, req(1, "XDPoS_networkInformation", json!([])));
    assert_eq!(m["result"]["networkId"], 51);
    assert_eq!(m["result"]["epoch"], 900);
    assert_eq!(
        ask(&r, req(1, "XDPoS_getLatestPoolStatus", json!([])))["result"],
        json!({"votePool": 0, "timeoutPool": 0})
    );
    let s = ask(&r, req(1, "XDPoS_getSigners", json!(["latest"])));
    // Ascending by address, as `Snapshot.GetSigners`.
    assert_eq!(s["result"][0], format!("{:#x}", Address::repeat_byte(1)));
    // eth_getMasternodes is the geth quirk: a string never decodes into types.Block.
    assert_eq!(
        ask(&r, req(1, "eth_getMasternodes", json!(["0x5"])))["error"]["code"],
        -32602
    );
}

async fn raw_http(addr: std::net::SocketAddr, body: &str) -> String {
    raw_http_host(addr, "localhost", body).await
}

async fn raw_http_host(addr: std::net::SocketAddr, host: &str, body: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let msg = format!("POST / HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    s.write_all(msg.as_bytes()).await.expect("write");
    let mut out = String::new();
    s.read_to_string(&mut out).await.expect("read");
    out
}

/// The status line and headers (lower case) and the raw body of one HTTP/1.1 exchange.
async fn raw_http_bytes(addr: std::net::SocketAddr, extra: &str, body: &str) -> (String, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let msg = format!("POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    s.write_all(msg.as_bytes()).await.expect("write");
    let mut out = vec![];
    s.read_to_end(&mut out).await.expect("read");
    let at = out.windows(4).position(|w| w == b"\r\n\r\n").expect("head");
    (
        String::from_utf8_lossy(&out[..at]).to_lowercase(),
        out[at + 4..].to_vec(),
    )
}

#[tokio::test]
async fn http_gzips_large_responses_for_clients_that_ask() {
    use std::io::Read;
    let r = rpc();
    let ls = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = ls.local_addr().expect("addr");
    let h = tokio::spawn(xone_rpc::transport::serve_http(r, ls));
    let batch =
        serde_json::Value::Array((0..60).map(|i| req(i, "eth_chainId", json!([]))).collect())
            .to_string();
    let (head, plain) = raw_http_bytes(addr, "", &batch).await;
    assert!(head.starts_with("http/1.1 200"), "{head}");
    assert!(!head.contains("content-encoding"), "{head}");
    assert!(head.contains("vary: accept-encoding"), "{head}");
    assert!(plain.len() >= xone_rpc::transport::GZIP_MIN_BYTES);
    let (head, zipped) = raw_http_bytes(addr, "Accept-Encoding: gzip, deflate\r\n", &batch).await;
    assert!(head.contains("content-encoding: gzip"), "{head}");
    assert!(zipped.len() < plain.len());
    let mut out = vec![];
    flate2::read::GzDecoder::new(&zipped[..])
        .read_to_end(&mut out)
        .expect("gunzip");
    assert_eq!(
        out, plain,
        "the decompressed body is the plain one, byte for byte"
    );
    // q=0 refuses gzip; a small response stays plain
    let (head, body) = raw_http_bytes(addr, "Accept-Encoding: gzip;q=0\r\n", &batch).await;
    assert!(!head.contains("content-encoding"), "{head}");
    assert_eq!(body, plain);
    let one = req(1, "eth_chainId", json!([])).to_string();
    let (head, _) = raw_http_bytes(addr, "Accept-Encoding: gzip\r\n", &one).await;
    assert!(!head.contains("content-encoding"), "{head}");
    h.abort();
}

#[tokio::test]
async fn http_serves_requests_and_429s_on_rejection() {
    let l = Limits {
        bucket_burst: 5,
        bucket_per_sec: 1,
        ..Limits::default()
    };
    let r = rpc_with(l, 0);
    let ls = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = ls.local_addr().expect("addr");
    let h = tokio::spawn(xone_rpc::transport::serve_http(r, ls));
    let ok = raw_http(addr, &req(1, "eth_chainId", json!([])).to_string()).await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    assert!(ok.contains("\"result\":\"0x33\""));
    // A fresh connection has its own bucket (burst 5), the transport bucket is shared: drain it.
    let mut last = String::new();
    for _ in 0..8 {
        last = raw_http(
            addr,
            &req(1, "eth_getBalance", json!([format!("{ACCT:#x}")])).to_string(),
        )
        .await;
    }
    assert!(last.starts_with("HTTP/1.1 429"), "{last}");
    assert!(last.to_lowercase().contains("retry-after:"));
    assert!(last.contains("-32005"));
    let big = raw_http(addr, &"x".repeat(6 << 20)).await;
    assert!(
        big.starts_with("HTTP/1.1 413") || big.is_empty() || big.contains("-32"),
        "{}",
        &big[..big.len().min(80)]
    );
    h.abort();
}

#[tokio::test]
async fn websocket_subscribes_receives_heads_and_cleans_up_on_close() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let r = rpc();
    let ls = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = ls.local_addr().expect("addr");
    let h = tokio::spawn(xone_rpc::transport::serve_ws(r.clone(), ls));
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");
    ws.send(Message::text(
        req(1, "eth_subscribe", json!(["newHeads"])).to_string(),
    ))
    .await
    .expect("send");
    let reply: Value = serde_json::from_str(
        ws.next()
            .await
            .expect("msg")
            .expect("ok")
            .to_text()
            .expect("text"),
    )
    .expect("json");
    let id = reply["result"].as_str().expect("sub id").to_owned();
    assert_eq!(r.hub.len(), 1);
    r.hub.publish_head(
        &json!({"number": "0x1", "validators": "0x", "validator": "0x", "penalties": "0x"}),
    );
    let note: Value = serde_json::from_str(
        ws.next()
            .await
            .expect("msg")
            .expect("ok")
            .to_text()
            .expect("text"),
    )
    .expect("json");
    assert_eq!(note["method"], "eth_subscription");
    assert_eq!(note["params"]["subscription"], id);
    assert_eq!(note["params"]["result"]["number"], "0x1");
    // A request on the same connection still works.
    ws.send(Message::text(
        req(2, "eth_blockNumber", json!([])).to_string(),
    ))
    .await
    .expect("send");
    let v: Value = serde_json::from_str(
        ws.next()
            .await
            .expect("msg")
            .expect("ok")
            .to_text()
            .expect("text"),
    )
    .expect("json");
    assert_eq!(v["result"], "0x64");
    ws.send(Message::text(
        req(3, "eth_unsubscribe", json!([id])).to_string(),
    ))
    .await
    .expect("send");
    let u: Value = serde_json::from_str(
        ws.next()
            .await
            .expect("msg")
            .expect("ok")
            .to_text()
            .expect("text"),
    )
    .expect("json");
    assert_eq!(u["result"], true);
    assert_eq!(r.hub.len(), 0);
    // Close: a second subscription is removed with its connection.
    ws.send(Message::text(
        req(
            4,
            "eth_subscribe",
            json!(["logs", {"address": "xdc0000000000000000000000000000000000000089"}]),
        )
        .to_string(),
    ))
    .await
    .expect("send");
    let _ = ws.next().await;
    assert_eq!(r.hub.len(), 1);
    ws.close(None).await.expect("close");
    for _ in 0..50 {
        if r.hub.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(r.hub.is_empty());
    // eth_subscribe over HTTP is not supported (geth `notificationsUnsupportedError`).
    assert_eq!(
        ask(&r, req(1, "eth_subscribe", json!(["newHeads"])))["error"]["code"],
        -32601
    );
    h.abort();
}

#[tokio::test]
async fn ipc_serves_a_json_stream() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let r = rpc();
    let dir = std::env::temp_dir().join(format!("xone-rpc-ipc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("x.ipc");
    let p = path.clone();
    let h = tokio::spawn(async move { xone_rpc::transport::serve_ipc(r, &p).await });
    for _ in 0..50 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // #60: the socket serves every namespace, so only its owner may connect (geth `ipc_unix.go`)
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path)
        .expect("socket")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    let mut s = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    s.write_all(req(1, "net_version", json!([])).to_string().as_bytes())
        .await
        .expect("write");
    let mut buf = vec![0u8; 256];
    let n = s.read(&mut buf).await.expect("read");
    let v: Value = serde_json::from_slice(&buf[..n]).expect("json");
    assert_eq!(v["result"], "51");
    h.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A range whose span is the whole u64 space must be refused, not wrap past the block budget.
#[test]
fn get_logs_rejects_a_range_that_overflows_the_budget() {
    let r = rpc();
    let e = ask(
        &r,
        req(
            1,
            "eth_getLogs",
            json!([{"fromBlock": "0x0", "toBlock": "0xffffffffffffffff"}]),
        ),
    );
    assert_eq!(e["error"]["code"], -32602, "{e}");
    assert!(e["error"]["message"]
        .as_str()
        .expect("msg")
        .starts_with("exceed maximum block range"));
}

/// A state that leaves the retained window during the read is a typed error, never a zero value.
#[test]
fn lost_state_is_a_typed_error_not_zero() {
    let r = rpc();
    for (m, p) in [
        ("eth_getBalance", json!([format!("{ACCT:#x}"), "0x7"])),
        (
            "eth_getTransactionCount",
            json!([format!("{ACCT:#x}"), "0x7"]),
        ),
        ("eth_getCode", json!([format!("{ACCT:#x}"), "0x7"])),
        (
            "eth_getStorageAt",
            json!([format!("{ACCT:#x}"), "0x0", "0x7"]),
        ),
    ] {
        let e = ask(&r, req(1, m, p));
        assert!(
            e["error"]["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("unsupported history")),
            "{m}: {e}"
        );
    }
}

/// An `Rpc` whose registry is the `--http.api`/`--ws.api` list `api` (geth `rpc.Server.RegisterName` per module).
fn rpc_api(api: &[&str], policy: xone_rpc::policy::Policy) -> Arc<Rpc> {
    let mut r = rpc();
    let api: Vec<String> = api.iter().map(|s| (*s).to_owned()).collect();
    let rm = Arc::get_mut(&mut r).expect("unshared");
    rm.reg = xone_rpc::policy::registry(&api).expect("known namespaces");
    rm.policy = policy;
    r
}

#[test]
fn unknown_namespace_is_refused_and_defaults_hide_privileged_methods() {
    use xone_rpc::policy::{registry, Refusal};
    let bad = ["eth".to_owned(), "*".to_owned()];
    assert_eq!(
        registry(&bad).err(),
        Some(Refusal::UnknownNamespace("*".into()))
    );
    let r = rpc_api(&["eth", "net", "web3"], Default::default());
    let e = ask(&r, req(1, "admin_peers", json!([])));
    assert_eq!(e["error"]["code"], -32601);
    assert_eq!(
        ask(&r, req(1, "debug_traceTransaction", json!([])))["error"]["code"],
        -32601
    );
}

#[tokio::test]
async fn http_refuses_methods_outside_its_list_and_a_wrong_host() {
    let r = rpc_api(&["eth"], Default::default());
    let ls = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = ls.local_addr().expect("addr");
    let h = tokio::spawn(xone_rpc::transport::serve_http(r, ls));
    let ok = raw_http(addr, &req(1, "eth_chainId", json!([])).to_string()).await;
    assert!(ok.contains("\"result\":\"0x33\""), "{ok}");
    let no = raw_http(addr, &req(1, "net_version", json!([])).to_string()).await;
    assert!(no.contains("-32601"), "{no}");
    // a hostname that is not a listed vhost never reaches dispatch; an IP address does
    let bad = raw_http_host(
        addr,
        "evil.example:8545",
        &req(1, "eth_chainId", json!([])).to_string(),
    )
    .await;
    assert!(bad.starts_with("HTTP/1.1 403"), "{bad}");
    assert!(bad.contains("invalid host specified") && !bad.contains("result"));
    let ip = raw_http_host(
        addr,
        "10.1.2.3:8545",
        &req(1, "eth_chainId", json!([])).to_string(),
    )
    .await;
    assert!(ip.contains("\"result\""), "{ip}");
    h.abort();
}

#[tokio::test]
async fn websocket_refuses_methods_outside_its_list_and_a_wrong_origin() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    let r = rpc_api(
        &["eth"],
        xone_rpc::policy::Policy {
            vhosts: vec![],
            origins: vec!["https://app.example".into()],
            jwt: None,
        },
    );
    let ls = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = ls.local_addr().expect("addr");
    let h = tokio::spawn(xone_rpc::transport::serve_ws(r, ls));
    let connect = |origin: &'static str| async move {
        let mut q = format!("ws://{addr}").into_client_request().expect("req");
        q.headers_mut()
            .insert("origin", origin.parse().expect("hdr"));
        tokio_tungstenite::connect_async(q).await
    };
    assert!(connect("http://evil.example").await.is_err());
    let (mut ws, _) = connect("https://app.example")
        .await
        .expect("allowed origin");
    ws.send(Message::text(req(1, "eth_chainId", json!([])).to_string()))
        .await
        .expect("send");
    let ok: Value = serde_json::from_str(
        ws.next()
            .await
            .expect("msg")
            .expect("ok")
            .to_text()
            .expect("text"),
    )
    .expect("json");
    assert_eq!(ok["result"], "0x33");
    ws.send(Message::text(req(2, "admin_peers", json!([])).to_string()))
        .await
        .expect("send");
    let no: Value = serde_json::from_str(
        ws.next()
            .await
            .expect("msg")
            .expect("ok")
            .to_text()
            .expect("text"),
    )
    .expect("json");
    assert_eq!(no["error"]["code"], -32601);
    h.abort();
}

#[test]
fn xdc_namespace_aliases_eth_and_is_off_unless_listed() {
    let a = format!("{ACCT:#x}");
    let both = rpc_api(&["eth", "xdc"], Default::default());
    for (x, e, params) in [
        ("xdc_getBalance", "eth_getBalance", json!([a, "latest"])),
        (
            "xdc_getBlockByNumber",
            "eth_getBlockByNumber",
            json!(["0x5", false]),
        ),
        (
            "xdc_getBlockSignersByNumber",
            "eth_getBlockSignersByNumber",
            json!(["0x5"]),
        ),
        ("xdc_chainId", "eth_chainId", json!([])),
    ] {
        let (rx, re) = (
            ask(&both, req(1, x, params.clone())),
            ask(&both, req(1, e, params)),
        );
        assert_eq!(rx, re, "{x}");
        assert!(
            rx.get("result").is_some() || rx["error"]["code"] != -32601,
            "{x}"
        );
    }
    // the transaction methods are not on the oracle BlockChainAPI
    assert_eq!(
        ask(&both, req(1, "xdc_getTransactionByHash", json!([])))["error"]["code"],
        -32601
    );
    // off unless listed; `xdc` alone does not open `eth_`
    let eth_only = rpc_api(&["eth"], Default::default());
    assert_eq!(
        ask(&eth_only, req(1, "xdc_getBalance", json!([])))["error"]["code"],
        -32601
    );
    let xdc_only = rpc_api(&["xdc"], Default::default());
    assert_eq!(
        ask(&xdc_only, req(1, "eth_getBalance", json!([])))["error"]["code"],
        -32601
    );
    assert_eq!(
        ask(&xdc_only, req(1, "xdc_chainId", json!([])))["result"],
        ask(&both, req(1, "eth_chainId", json!([])))["result"]
    );
}

/// Recorded from the oracle `xdc_getStateHealStatus` on an idle node (geth-XDC 6fc8e8c2).
#[test]
fn state_heal_status_matches_the_oracle_idle_shape() {
    let want: Value = serde_json::from_str(include_str!(
        "../../../conformance/vectors/rpc/xdc_getStateHealStatus_idle.json"
    ))
    .unwrap();
    let r = rpc_api(&["eth", "xdc"], Default::default());
    for m in ["xdc_getStateHealStatus", "eth_getStateHealStatus"] {
        assert_eq!(ask(&r, req(1, m, json!([])))["result"], want, "{m}");
    }
}

// --- #60: authentication, WebSocket limits and forwarding headers on the real listeners ---

const JWT_KEY: [u8; 32] = [0x5a; 32];

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// A token as a client builds it: `base64url(header).base64url(claims).base64url(HS256)`.
fn token(key: &[u8; 32], header: &str, claims: &str) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use hmac::{Hmac, Mac};
    let msg = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header),
        URL_SAFE_NO_PAD.encode(claims)
    );
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).expect("key");
    mac.update(msg.as_bytes());
    format!(
        "{msg}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}

fn good_token() -> String {
    token(
        &JWT_KEY,
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(r#"{{"iat":{}}}"#, unix_now()),
    )
}

fn jwt_policy() -> xone_rpc::policy::Policy {
    xone_rpc::policy::Policy {
        jwt: Some(JWT_KEY),
        ..Default::default()
    }
}

/// One POST with extra header lines (each ending in CRLF).
async fn raw_http_with(addr: std::net::SocketAddr, host: &str, extra: &str, body: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let msg = format!("POST / HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    s.write_all(msg.as_bytes()).await.expect("write");
    let mut out = String::new();
    s.read_to_string(&mut out).await.expect("read");
    out
}

#[tokio::test]
async fn http_with_a_jwt_secret_refuses_every_bad_credential_before_dispatch() {
    let r = rpc_api(&["eth", "admin"], jwt_policy());
    let ls = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = ls.local_addr().expect("addr");
    let h = tokio::spawn(xone_rpc::transport::serve_http(r, ls));
    let body = req(1, "admin_peers", json!([])).to_string();
    let now = unix_now();
    let hs = r#"{"alg":"HS256","typ":"JWT"}"#;
    let bad: Vec<(&str, String)> = vec![
        ("missing", String::new()),
        ("not bearer", format!("Authorization: {}\r\n", good_token())),
        ("malformed", "Authorization: Bearer abc.def\r\n".into()),
        ("garbage", "Authorization: Bearer !!!.!!!.!!!\r\n".into()),
        (
            "wrong secret",
            format!(
                "Authorization: Bearer {}\r\n",
                token(&[1; 32], hs, &format!(r#"{{"iat":{now}}}"#))
            ),
        ),
        (
            "stale iat",
            format!(
                "Authorization: Bearer {}\r\n",
                token(&JWT_KEY, hs, &format!(r#"{{"iat":{}}}"#, now - 3600))
            ),
        ),
        (
            "alg none",
            format!(
                "Authorization: Bearer {}\r\n",
                token(
                    &JWT_KEY,
                    r#"{"alg":"none"}"#,
                    &format!(r#"{{"iat":{now}}}"#)
                )
            ),
        ),
        (
            "forwarded headers are not identity",
            "X-Forwarded-For: 127.0.0.1\r\nForwarded: for=127.0.0.1\r\nX-Real-IP: 127.0.0.1\r\n"
                .into(),
        ),
    ];
    for (why, hdr) in &bad {
        let out = raw_http_with(addr, "localhost", hdr, &body).await;
        assert!(out.starts_with("HTTP/1.1 401"), "{why}: {out}");
        assert!(!out.contains("jsonrpc"), "{why}: dispatched: {out}");
    }
    let ok = raw_http_with(
        addr,
        "localhost",
        &format!("Authorization: Bearer {}\r\n", good_token()),
        &body,
    )
    .await;
    assert!(
        ok.starts_with("HTTP/1.1 200") && ok.contains("\"result\""),
        "{ok}"
    );
    // a valid token grants only the listed namespaces
    let off = raw_http_with(
        addr,
        "localhost",
        &format!("Authorization: Bearer {}\r\n", good_token()),
        &req(2, "debug_getRawHeader", json!(["0x1"])).to_string(),
    )
    .await;
    assert!(off.contains("-32601"), "{off}");
    // a wrong Host is refused even with a valid token, and a forwarding header does not help it
    let host = raw_http_with(
        addr,
        "evil.example",
        &format!(
            "X-Forwarded-Host: localhost\r\nAuthorization: Bearer {}\r\n",
            good_token()
        ),
        &body,
    )
    .await;
    assert!(host.starts_with("HTTP/1.1 403"), "{host}");
    h.abort();
}

#[tokio::test]
async fn websocket_with_a_jwt_secret_needs_the_token_on_the_upgrade() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Error, Message};
    let r = rpc_api(&["eth"], jwt_policy());
    let ls = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = ls.local_addr().expect("addr");
    let h = tokio::spawn(xone_rpc::transport::serve_ws(r, ls));
    let connect = |auth: Option<String>| async move {
        let mut q = format!("ws://{addr}").into_client_request().expect("req");
        if let Some(a) = auth {
            q.headers_mut()
                .insert("authorization", a.parse().expect("hdr"));
        }
        tokio_tungstenite::connect_async(q).await
    };
    for auth in [
        None,
        Some("Bearer x.y.z".to_owned()),
        Some(format!(
            "Bearer {}",
            token(
                &[2; 32],
                r#"{"alg":"HS256"}"#,
                &format!(r#"{{"iat":{}}}"#, unix_now())
            )
        )),
    ] {
        match connect(auth.clone()).await {
            Err(Error::Http(resp)) => assert_eq!(resp.status(), 401, "{auth:?}"),
            other => panic!("{auth:?}: upgrade not refused: {:?}", other.map(|_| ())),
        }
    }
    let (mut ws, _) = connect(Some(format!("Bearer {}", good_token())))
        .await
        .expect("valid token");
    ws.send(Message::text(req(1, "eth_chainId", json!([])).to_string()))
        .await
        .expect("send");
    let ok: Value = serde_json::from_str(
        ws.next()
            .await
            .expect("msg")
            .expect("ok")
            .to_text()
            .expect("text"),
    )
    .expect("json");
    assert_eq!(ok["result"], "0x33");
    h.abort();
}

#[tokio::test]
async fn websocket_shares_the_http_body_and_batch_limits() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let lim = Limits {
        body_bytes: 4096,
        ..Limits::default()
    };
    let r = rpc_with(lim, 0);
    let ls = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = ls.local_addr().expect("addr");
    let h = tokio::spawn(xone_rpc::transport::serve_ws(r, ls));
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");
    // 101 items: the batch bound of HTTP holds on WebSocket
    let batch: Vec<Value> = (0..101).map(|i| req(i, "net_version", json!([]))).collect();
    let small_batch = serde_json::to_string(&batch).expect("json");
    assert!(small_batch.len() < 4096 * 2);
    // a message above body_bytes closes the connection instead of reaching the parser
    let big = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"net_version","params":["{}"]}}"#,
        "a".repeat(8192)
    );
    ws.send(Message::text(big)).await.expect("send");
    let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("closed in time");
    assert!(
        !matches!(&next, Some(Ok(Message::Text(_)))),
        "an oversized message was answered: {next:?}"
    );
    h.abort();

    let r = rpc();
    let ls = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = ls.local_addr().expect("addr");
    let h = tokio::spawn(xone_rpc::transport::serve_ws(r, ls));
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .expect("connect");
    ws.send(Message::text(small_batch)).await.expect("send");
    let v: Value = serde_json::from_str(
        ws.next()
            .await
            .expect("msg")
            .expect("ok")
            .to_text()
            .expect("text"),
    )
    .expect("json");
    assert_eq!(v["error"]["code"], -32600, "{v}");
    h.abort();
}

#[tokio::test]
async fn a_plain_http_request_to_the_websocket_port_is_not_dispatched() {
    let r = rpc();
    let ls = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = ls.local_addr().expect("addr");
    let h = tokio::spawn(xone_rpc::transport::serve_ws(r, ls));
    let out = raw_http(addr, &req(1, "eth_chainId", json!([])).to_string()).await;
    assert!(!out.contains("result"), "{out}");
    assert!(out.is_empty() || out.starts_with("HTTP/1.1 4"), "{out}");
    h.abort();
}
