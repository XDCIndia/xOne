//! Exporters of the XB-08 vectors in `conformance/vectors/special/` from the Apothem archive. Every
//! expected value in them is an archive answer (receipts, balances, headers, `eth_call` results,
//! `prestateTracer` storage); nothing is derived by xone code except where a vector says so (the
//! batching of reads and the receipts-root self-check that drops a block whose archive receipts do
//! not hash to the header).

use crate::{
    case::Case,
    fetch::{author, fetch_case, header_of, hex_u64, Rpc},
};
use alloy_primitives::{keccak256, Address, Bytes, Log, B256, U256};
use serde_json::{json, Map, Value};
use std::{fs, io::Write, path::PathBuf, str::FromStr};
use xone_chainspec::{ForkBlock, Network};
use xone_evm::{route::is_special_tx, special::Receipt};
use xone_primitives::Header;
use xone_state::system_contracts::{randomize_0x90, signer_0x89, validator_0x88};

/// Newest block the exporters draw from (the archive head at export time, with margin).
const HEAD: u64 = 85_200_000;

pub fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/special")
}

fn write_lines(name: &str, lines: &[Value]) {
    fs::create_dir_all(dir()).unwrap();
    let mut f = fs::File::create(dir().join(name)).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
    eprintln!("{name}: {} lines", lines.len());
}

struct Lcg(u64);
impl Lcg {
    fn between(&mut self, lo: u64, hi: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        lo + (self.0 >> 24) % (hi - lo)
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}
fn hx(n: u64) -> String {
    format!("{n:#x}")
}
fn at(f: ForkBlock) -> u64 {
    match f {
        ForkBlock::At(n) => n,
        _ => panic!("fork row is not scheduled"),
    }
}
fn adr(v: &Value) -> Address {
    Address::from_str(v.as_str().unwrap_or_default()).unwrap_or_default()
}

/// Several answers in one request; `Null` for a failed call.
fn many(rpc: &mut Rpc, calls: Vec<(&str, Value)>) -> Vec<Value> {
    if calls.is_empty() {
        return vec![];
    }
    let n = calls.len();
    rpc.batch(calls)
        .unwrap_or_else(|e| {
            eprintln!("request failed: {e}");
            vec![Value::Null; n]
        })
        .into_iter()
        .map(|a| a["result"].clone())
        .collect()
}

fn receipt_of(ty: u8, r: &Value) -> Receipt {
    let logs = r["logs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| {
            let topics = l["topics"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| B256::from_str(t.as_str().unwrap_or_default()).unwrap_or_default())
                .collect();
            let data = Bytes::from_str(l["data"].as_str().unwrap_or("0x")).unwrap_or_default();
            Log::new_unchecked(adr(&l["address"]), topics, data)
        })
        .collect();
    Receipt::new(
        ty,
        hex_u64(&r["status"]) == 1,
        hex_u64(&r["cumulativeGasUsed"]),
        hex_u64(&r["gasUsed"]),
        logs,
    )
}

fn receipt_json(r: &Value) -> Value {
    json!({
        "status": hex_u64(&r["status"]), "gasUsed": hex_u64(&r["gasUsed"]),
        "cumulativeGasUsed": hex_u64(&r["cumulativeGasUsed"]), "logsBloom": r["logsBloom"],
        "effectiveGasPrice": r["effectiveGasPrice"],
        "logs": r["logs"].as_array().into_iter().flatten()
            .map(|l| json!({"address": l["address"], "topics": l["topics"], "data": l["data"]}))
            .collect::<Vec<_>>(),
    })
}

/// `(block with transactions, its receipts)` for each number that the archive answers.
fn blocks_with_receipts(rpc: &mut Rpc, nums: &[u64]) -> Vec<(Value, Value)> {
    let mut calls = vec![];
    for n in nums {
        calls.push(("eth_getBlockByNumber", json!([hx(*n), true])));
        calls.push(("eth_getBlockReceipts", json!([hx(*n)])));
    }
    let a = many(rpc, calls);
    a.chunks(2)
        .filter(|c| c[0].is_object() && c[1].is_array())
        .map(|c| (c[0].clone(), c[1].clone()))
        .collect()
}

fn txs_of(b: &Value) -> Vec<Value> {
    b["transactions"].as_array().cloned().unwrap_or_default()
}

/// `true` when the archive receipts of the block, encoded by [`Receipt::encode`], hash to the
/// header's receipts root and every bloom equals the archive's.
fn receipts_hash_to_header(b: &Value, rs: &Value) -> bool {
    let txs = txs_of(b);
    let rs = rs.as_array().cloned().unwrap_or_default();
    if txs.len() != rs.len() {
        return false;
    }
    let mut enc = vec![];
    for (t, r) in txs.iter().zip(&rs) {
        let rc = receipt_of(hex_u64(&t["type"]) as u8, r);
        if format!("{:#x}", rc.bloom) != s(&r["logsBloom"]) {
            return false;
        }
        enc.push(rc.encode());
    }
    format!("{:#x}", alloy_trie::root::ordered_trie_root_encoded(&enc)) == s(&b["receiptsRoot"])
}

fn is_sign(t: &Value) -> bool {
    adr(&t["to"]) == signer_0x89::ADDRESS
}

/// `sign_blocks.jsonl`: whole Apothem blocks that carry sign transactions, with the archive
/// receipts of every transaction, for the three eras after `TIPSigning`. The test replaces the sign
/// receipts by xone's own and recomputes the receipts root.
pub fn sign_blocks(rpc: &mut Rpc, per: usize, seed: u64) {
    let spec = Network::Apothem.spec();
    let tip = at(spec.effective.tip_signing);
    let trc21 = at(spec.effective.tip_trc21_fee);
    let v2 = spec.xdpos.v2.as_ref().unwrap().switch_block;
    let strata = [
        ("v1-before-tiptrc21fee", tip, trc21),
        ("v1-after-tiptrc21fee", trc21, v2 + 1),
        ("v2", v2 + 1, HEAD),
    ];
    let mut rng = Lcg(seed);
    let mut out = vec![];
    let mut seen = std::collections::BTreeSet::new();
    let (mut mismatch, mut signs) = (0, 0);
    for (name, lo, hi) in strata {
        let mut got = 0;
        let mut fixed: Vec<u64> = if name == "v1-before-tiptrc21fee" {
            (tip - 1..tip + 12).collect()
        } else if name == "v1-after-tiptrc21fee" {
            (trc21 - 6..trc21 + 6).collect()
        } else {
            vec![]
        };
        for _round in 0..200 {
            if got >= per {
                break;
            }
            let nums: Vec<u64> = if fixed.is_empty() {
                (0..25).map(|_| rng.between(lo, hi)).collect()
            } else {
                std::mem::take(&mut fixed)
            };
            for (b, rs) in blocks_with_receipts(rpc, &nums) {
                let n = hex_u64(&b["number"]);
                let txs = txs_of(&b);
                if txs.is_empty() || txs.len() > 30 || !txs.iter().any(is_sign) || !seen.insert(n) {
                    continue;
                }
                if !receipts_hash_to_header(&b, &rs) {
                    mismatch += 1;
                    continue;
                }
                // Senders whose every transaction of the block is a sign transaction.
                let mut senders: Vec<String> = txs
                    .iter()
                    .map(|t| s(&t["from"]))
                    .filter(|f| txs.iter().filter(|t| s(&t["from"]) == *f).all(is_sign))
                    .collect();
                senders.sort();
                senders.dedup();
                senders.truncate(2);
                let mut calls = vec![];
                for f in &senders {
                    calls.push(("eth_getTransactionCount", json!([f, hx(n - 1)])));
                    calls.push(("eth_getTransactionCount", json!([f, hx(n)])));
                }
                let nonces = many(rpc, calls);
                let nonces: Vec<Value> = senders
                    .iter()
                    .zip(nonces.chunks(2))
                    .map(|(f, c)| json!({"from": f, "before": hex_u64(&c[0]), "after": hex_u64(&c[1])}))
                    .collect();
                signs += txs.iter().filter(|t| is_sign(t)).count();
                out.push(json!({
                    "network": "apothem", "stratum": name, "number": n, "hash": b["hash"],
                    "receiptsRoot": b["receiptsRoot"],
                    "txs": txs.iter().zip(rs.as_array().unwrap()).map(|(t, r)| json!({
                        "ty": hex_u64(&t["type"]), "from": t["from"], "to": t["to"],
                        "nonce": hex_u64(&t["nonce"]), "receipt": receipt_json(r)})).collect::<Vec<_>>(),
                    "nonces": nonces,
                }));
                got += 1;
            }
        }
        eprintln!("{name}: {got} blocks");
    }
    eprintln!(
        "{signs} sign txs, {mismatch} blocks dropped (archive receipts do not hash to the header)"
    );
    out.sort_by_key(|v| v["number"].as_u64());
    write_lines("sign_blocks.jsonl", &out);
}

fn write_case(sub: &str, c: &Case) {
    let d = dir().join("cases");
    fs::create_dir_all(&d).unwrap();
    let _ = sub;
    fs::write(
        d.join(format!("{}.json", c.name)),
        serde_json::to_vec(c).unwrap(),
    )
    .unwrap();
}

/// `cases/*.json`: single transactions for the sweep's special mode, replayed through the router:
/// sign transactions before `TIPSigning` (EVM, gas price 0), after it (no EVM), randomize
/// transactions (EVM, fee exempt) and XDCx empty transactions.
pub fn cases(rpc: &mut Rpc, seed: u64) {
    let spec = Network::Apothem.spec();
    let tip = at(spec.effective.tip_signing);
    let trc21 = at(spec.effective.tip_trc21_fee);
    let v2 = spec.xdpos.v2.as_ref().unwrap().switch_block;
    let xdcx_end = at(spec.effective.tip_xdcx_receiver_disable);
    let mut rng = Lcg(seed);
    // (class, wanted, lo, hi, predicate on the target address)
    type Pick = fn(&Address) -> bool;
    let plan: [(&str, usize, u64, u64, Pick); 6] = [
        ("sign-evm", 40, 1000, tip, |a| *a == signer_0x89::ADDRESS),
        ("sign-v1-before-trc21", 10, tip, trc21, |a| {
            *a == signer_0x89::ADDRESS
        }),
        ("sign-v1-after-trc21", 10, trc21, v2 + 1, |a| {
            *a == signer_0x89::ADDRESS
        }),
        ("sign-v2", 10, v2 + 1, HEAD, |a| *a == signer_0x89::ADDRESS),
        ("randomize", 10, at(spec.effective.tip_randomize), v2, |a| {
            *a == randomize_0x90::ADDRESS
        }),
        (
            "xdcx-empty",
            8,
            at(spec.effective.tip_xdcx),
            xdcx_end,
            |a| {
                [0x91u8, 0x92, 0x93, 0x94]
                    .iter()
                    .any(|b| a.as_slice()[19] == *b && a.as_slice()[..19] == [0; 19])
            },
        ),
    ];
    for (class, want, lo, hi, pick) in plan {
        let mut got = 0;
        for _ in 0..150 {
            if got >= want {
                break;
            }
            let mut nums: Vec<u64> = (0..30).map(|_| rng.between(lo, hi)).collect();
            if class == "randomize" {
                // The secret and opening transactions are sent at the end of an epoch.
                let e = spec.xdpos.epoch;
                nums = nums.iter().map(|n| n / e * e + 800 + n % 100).collect();
            }
            let calls = nums
                .iter()
                .map(|n| ("eth_getBlockByNumber", json!([hx(*n), true])))
                .collect();
            for b in many(rpc, calls) {
                let Some(t) = txs_of(&b).into_iter().find(|t| pick(&adr(&t["to"]))) else {
                    continue;
                };
                if got >= want {
                    break;
                }
                match fetch_case(rpc, "apothem", &b, &t) {
                    Ok(Ok(mut c)) => {
                        c.name = format!("{class}-{}-{}", c.block.number, c.tx.index);
                        c.coupling = Some(class.to_string());
                        write_case(class, &c);
                        got += 1;
                    }
                    Ok(Err(w)) => eprintln!("{class}: excluded: {w}"),
                    Err(e) => eprintln!("{class}: {e}"),
                }
            }
        }
        eprintln!("{class}: {got} of {want}");
    }
}

fn json_file(name: &str, v: &Value) {
    fs::create_dir_all(dir()).unwrap();
    fs::write(dir().join(name), serde_json::to_vec(v).unwrap()).unwrap();
    eprintln!("{name} written");
}

/// `tip_signing_apothem.json`: every account of the state after block `TIPSigning - 1`
/// (`debug_accountRange`, no storage, with each account's storage root) and the two state roots.
pub fn tip_signing(rpc: &mut Rpc) {
    let spec = Network::Apothem.spec();
    let n = at(spec.effective.tip_signing);
    let hdr = many(
        rpc,
        vec![
            ("eth_getBlockByNumber", json!([hx(n - 1), false])),
            ("eth_getBlockByNumber", json!([hx(n), true])),
        ],
    );
    let mut accounts = vec![];
    let mut start = format!("{:#x}", B256::ZERO);
    loop {
        let r = many(
            rpc,
            vec![(
                "debug_accountRange",
                json!([hx(n - 1), start, 256, true, true, true]),
            )],
        );
        for (_, a) in r[0]["accounts"].as_object().into_iter().flatten() {
            accounts.push(json!({
                "key": a["key"], "address": a["address"], "nonce": a["nonce"],
                "balance": a["balance"], "root": a["root"], "codeHash": a["codeHash"]}));
        }
        match r[0]["next"].as_str() {
            Some(nx) if !nx.is_empty() && accounts.len() < 100_000 => start = nx.to_string(),
            _ => break,
        }
    }
    // The API shows a balance wider than 256 bits cut to its low bits (XDC wide balances), so the
    // leaf of each account's proof, which holds the real RLP, is kept as the account.
    for chunk in accounts.chunks_mut(25) {
        let r = many(
            rpc,
            chunk
                .iter()
                .map(|x| ("eth_getProof", json!([x["address"], [], hx(n - 1)])))
                .collect(),
        );
        for (x, p) in chunk.iter_mut().zip(r) {
            let node = p["accountProof"]
                .as_array()
                .and_then(|p| p.last())
                .map(s)
                .unwrap_or_default();
            let node = alloy_primitives::hex::decode(node).unwrap();
            let mut b = node.as_slice();
            alloy_rlp::Header::decode(&mut b).unwrap();
            alloy_rlp::Header::decode_bytes(&mut b, false).unwrap();
            let leaf = alloy_rlp::Header::decode_bytes(&mut b, false).unwrap();
            x["leaf"] = json!(format!("0x{}", alloy_primitives::hex::encode(leaf)));
        }
    }
    json_file(
        "tip_signing_apothem.json",
        &json!({
            "network": "apothem", "number": n,
            "parentStateRoot": hdr[0]["stateRoot"], "stateRoot": hdr[1]["stateRoot"],
            "transactions": txs_of(&hdr[1]).len(), "accounts": accounts,
        }),
    );
}

fn selector(sig: &str) -> String {
    format!("0x{}", alloy_primitives::hex::encode(&keccak256(sig)[..4]))
}
fn word_of(a: &Address) -> String {
    alloy_primitives::hex::encode(a.into_word())
}
/// ABI return of an address array: offset, length, elements.
fn decode_addresses(ret: &str) -> Vec<String> {
    let b = alloy_primitives::hex::decode(ret).unwrap_or_default();
    if b.len() < 64 {
        return vec![];
    }
    let n = U256::from_be_slice(&b[32..64]).saturating_to::<usize>();
    (0..n)
        .filter_map(|i| b.get(64 + 32 * i..96 + 32 * i))
        .map(|w| format!("{:#x}", Address::from_slice(&w[12..])))
        .collect()
}

/// The `prestateTracer` storage of `to` for a call at `block`, with the `eth_call` return data.
fn call_and_slots(rpc: &mut Rpc, block: u64, to: &str, data: &str) -> (String, Map<String, Value>) {
    let p = json!({"from": "0x00000000000000000000000000000000000000f1", "to": to, "data": data});
    let r = many(
        rpc,
        vec![
            ("eth_call", json!([p, hx(block)])),
            (
                "debug_traceCall",
                json!([p, hx(block), {"tracer": "prestateTracer"}]),
            ),
        ],
    );
    let slots = r[1][to.to_lowercase()]["storage"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    (s(&r[0]), slots)
}

/// `system_contracts.jsonl`: reads of 0x88 (candidates, owner, cap, voters, voter cap), 0x89
/// (signers of recent blocks, before the contract is wiped) and 0x90 (secrets, opening), each with the
/// archive's `eth_call` answer and the storage the call read (`debug_traceCall` `prestateTracer`),
/// which is the state the Rust reader runs on.
pub fn slots(rpc: &mut Rpc, per: usize, seed: u64) {
    let spec = Network::Apothem.spec();
    let tip = at(spec.effective.tip_signing);
    let mut rng = Lcg(seed);
    let (get_candidates, owner, cap, voters, voter_cap) = (
        selector("getCandidates()"),
        selector("getCandidateOwner(address)"),
        selector("getCandidateCap(address)"),
        selector("getVoters(address)"),
        selector("getVoterCap(address,address)"),
    );
    let (get_signers, secret, opening) = (
        selector("getSigners(bytes32)"),
        selector("getSecret(address)"),
        selector("getOpening(address)"),
    );
    let mut out = vec![];
    // 0x88 over the whole history.
    while out.len() < per {
        let n = rng.between(1, HEAD);
        let (ret, mut slots) = call_and_slots(
            rpc,
            n,
            "0x0000000000000000000000000000000000000088",
            &get_candidates,
        );
        let cands = decode_addresses(&ret);
        if cands.is_empty() {
            continue;
        }
        let mut reads = vec![];
        for i in [0, cands.len() / 2, cands.len() - 1] {
            let c = Address::from_str(&cands[i]).unwrap();
            let (o, s1) = call_and_slots(
                rpc,
                n,
                "0x0000000000000000000000000000000000000088",
                &format!("{owner}{}", word_of(&c)),
            );
            let (k, s2) = call_and_slots(
                rpc,
                n,
                "0x0000000000000000000000000000000000000088",
                &format!("{cap}{}", word_of(&c)),
            );
            let (v, s3) = call_and_slots(
                rpc,
                n,
                "0x0000000000000000000000000000000000000088",
                &format!("{voters}{}", word_of(&c)),
            );
            let vs = decode_addresses(&v);
            let mut vc = Value::Null;
            if let Some(v0) = vs.first() {
                let (x, s4) = call_and_slots(
                    rpc,
                    n,
                    "0x0000000000000000000000000000000000000088",
                    &format!(
                        "{voter_cap}{}{}",
                        word_of(&c),
                        word_of(&Address::from_str(v0).unwrap())
                    ),
                );
                slots.extend(s4);
                vc = json!({"voter": v0, "cap": x});
            }
            slots.extend(s1);
            slots.extend(s2);
            slots.extend(s3);
            reads.push(
                json!({"candidate": cands[i], "owner": o, "cap": k, "voters": vs, "voterCap": vc}),
            );
        }
        out.push(json!({"contract": "0x88", "number": n, "candidates": cands, "reads": reads, "slots": slots}));
    }
    // 0x89: the signers of the last blocks before a block of the first 3,000,000.
    let mut got = 0;
    while got < per {
        let n = rng.between(100, tip);
        let hs = many(
            rpc,
            (1..=6)
                .map(|k| ("eth_getBlockByNumber", json!([hx(n - k), false])))
                .collect(),
        );
        let mut reads = vec![];
        let mut slots = Map::new();
        for h in hs.iter().filter(|h| h.is_object()) {
            let (ret, sl) = call_and_slots(
                rpc,
                n,
                "0x0000000000000000000000000000000000000089",
                &format!("{get_signers}{}", &s(&h["hash"])[2..]),
            );
            let signers = decode_addresses(&ret);
            if !signers.is_empty() {
                reads.push(json!({"blockHash": h["hash"], "signers": signers}));
                slots.extend(sl);
            }
        }
        if !reads.is_empty() {
            out.push(json!({"contract": "0x89", "number": n, "reads": reads, "slots": slots}));
            got += 1;
        }
    }
    write_lines("system_contracts.jsonl", &out);
    out.clear();
    // 0x90: masternodes with a committed secret, in V1 epochs after TIPRandomize.
    let (lo, v2) = (
        at(spec.effective.tip_randomize),
        spec.xdpos.v2.as_ref().unwrap().switch_block,
    );
    let mut got = 0;
    for _ in 0..40 {
        if got >= per.min(12) {
            break;
        }
        let e = spec.xdpos.epoch;
        let n = rng.between(lo, v2) / e * e + 899;
        let (ret, _) = call_and_slots(
            rpc,
            n,
            "0x0000000000000000000000000000000000000088",
            &get_candidates,
        );
        let mut reads = vec![];
        let mut slots = Map::new();
        for c in decode_addresses(&ret).iter().take(12) {
            let w = word_of(&Address::from_str(c).unwrap());
            let (sec, s1) = call_and_slots(
                rpc,
                n,
                "0x0000000000000000000000000000000000000090",
                &format!("{secret}{w}"),
            );
            let (op, s2) = call_and_slots(
                rpc,
                n,
                "0x0000000000000000000000000000000000000090",
                &format!("{opening}{w}"),
            );
            let hexs = alloy_primitives::hex::decode(&sec).unwrap_or_default();
            let cnt = if hexs.len() >= 64 {
                U256::from_be_slice(&hexs[32..64]).saturating_to::<usize>()
            } else {
                0
            };
            let secrets: Vec<String> = (0..cnt)
                .filter_map(|i| hexs.get(64 + 32 * i..96 + 32 * i))
                .map(|x| format!("0x{}", alloy_primitives::hex::encode(x)))
                .collect();
            if !secrets.is_empty()
                || !op
                    .trim_start_matches("0x")
                    .trim_start_matches('0')
                    .is_empty()
            {
                reads.push(json!({"masternode": c, "secrets": secrets, "opening": op}));
                slots.extend(s1);
                slots.extend(s2);
            }
            if reads.len() >= 2 {
                break;
            }
        }
        if !reads.is_empty() {
            out.push(json!({"contract": "0x90", "number": n, "reads": reads, "slots": slots}));
            got += 1;
        }
    }
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(dir().join("system_contracts.jsonl"))
        .unwrap();
    for l in &out {
        writeln!(f, "{l}").unwrap();
    }
    eprintln!(
        "system_contracts.jsonl: {} lines of 0x90 appended",
        out.len()
    );
}

/// `fee_recipient.jsonl`: blocks with fee-paying transactions, the balances before and after of the
/// block's author and of its registered owner (from 0x88 in the parent state), and each
/// transaction's gas and effective price from the receipts. Blocks where either account is a party to
/// a transaction, or that are checkpoints (rewards), are not used: the whole balance change is then the fee.
pub fn fees(rpc: &mut Rpc, per: usize, seed: u64) {
    let spec = Network::Apothem.spec();
    let tip = at(spec.effective.tip_signing);
    let trc21 = at(spec.effective.tip_trc21_fee);
    let v2 = spec.xdpos.v2.as_ref().unwrap().switch_block;
    let (e1559, g2500) = (at(spec.eip1559), at(spec.gas2500x));
    let strata = [
        ("v1-before-tiptrc21fee", tip, trc21),
        ("v1-after-tiptrc21fee", trc21 + 1, v2 + 1),
        ("v2-no-base-fee", v2 + 1, e1559),
        ("v2-base-fee-12.5", e1559, g2500),
        ("v2-base-fee-625", g2500, HEAD),
        ("boundary-tiptrc21fee", trc21 - 300, trc21 + 301),
    ];
    let mut rng = Lcg(seed);
    let mut out = vec![];
    for (name, lo, hi) in strata {
        let mut got = 0;
        let mut fixed: Vec<u64> = if name.starts_with("boundary") {
            (lo..hi).collect()
        } else {
            vec![]
        };
        for _ in 0..300 {
            if got >= per && !name.starts_with("boundary") {
                break;
            }
            let nums: Vec<u64> = if name.starts_with("boundary") {
                if fixed.is_empty() {
                    break;
                } else {
                    let rest = fixed.split_off(fixed.len().min(25));
                    std::mem::replace(&mut fixed, rest)
                }
            } else {
                (0..25).map(|_| rng.between(lo, hi)).collect()
            };
            for (b, rs) in blocks_with_receipts(rpc, &nums) {
                let n = hex_u64(&b["number"]);
                let txs = txs_of(&b);
                let rs = rs.as_array().cloned().unwrap_or_default();
                let paying: Vec<&Value> = txs
                    .iter()
                    .zip(&rs)
                    .filter(|(t, r)| {
                        !is_special_tx(Some(adr(&t["to"])))
                            && hex_u64(&r["gasUsed"]) > 0
                            && hex_u64(&r["effectiveGasPrice"]) > 0
                    })
                    .map(|(t, _)| t)
                    .collect();
                if paying.is_empty()
                    || txs.len() > 20
                    || n.is_multiple_of(spec.xdpos.epoch)
                    || s(&b["validators"]).len() > 2
                {
                    continue;
                }
                let Ok(who) = author(&b) else {
                    eprintln!("block {n}: no author");
                    continue;
                };
                let slot = xone_state::system_contracts::mapping_slot(
                    who.into_word(),
                    validator_0x88::VALIDATORS_STATE,
                );
                let ow = many(
                    rpc,
                    vec![(
                        "eth_getStorageAt",
                        json!([
                            "0x0000000000000000000000000000000000000088",
                            format!("{:#066x}", slot),
                            hx(n - 1)
                        ]),
                    )],
                );
                let w = s(&ow[0]);
                let owner = Address::from_str(&format!("0x{}", &w[w.len().saturating_sub(40)..]))
                    .unwrap_or_default();
                // A special or empty transaction moves no value and pays nothing: its parties are free.
                let parties: Vec<Address> = txs
                    .iter()
                    .zip(&rs)
                    .filter(|(t, r)| {
                        !is_special_tx(Some(adr(&t["to"]))) && hex_u64(&r["gasUsed"]) > 0
                    })
                    .flat_map(|(t, _)| [adr(&t["from"]), adr(&t["to"])])
                    .collect();
                if parties.contains(&who) || parties.contains(&owner) {
                    eprintln!("block {n}: author or owner is a party");
                    continue;
                }
                let mut watch = vec![who];
                if !owner.is_zero() && owner != who {
                    watch.push(owner);
                }
                let mut calls = vec![];
                for a in &watch {
                    calls.push(("eth_getBalance", json!([format!("{a:#x}"), hx(n - 1)])));
                    calls.push(("eth_getBalance", json!([format!("{a:#x}"), hx(n)])));
                }
                let bal = many(rpc, calls);
                let balances: Map<String, Value> = watch
                    .iter()
                    .zip(bal.chunks(2))
                    .map(|(a, c)| (format!("{a:#x}"), json!([c[0], c[1]])))
                    .collect();
                out.push(json!({
                    "network": "apothem", "stratum": name, "number": n, "author": format!("{who:#x}"),
                    "owner": (!owner.is_zero()).then(|| format!("{owner:#x}")),
                    "baseFee": b["baseFeePerGas"],
                    "txs": txs.iter().zip(&rs).map(|(t, r)| json!({"to": t["to"], "gasUsed": hex_u64(&r["gasUsed"]),
                        "price": r["effectiveGasPrice"]})).collect::<Vec<_>>(),
                    "balances": balances,
                }));
                got += 1;
            }
        }
        eprintln!("{name}: {got} blocks");
    }
    out.sort_by_key(|v| v["number"].as_u64());
    write_lines("fee_recipient.jsonl", &out);
}

/// `base_fee.jsonl` and `base_fee_negative.jsonl`: the headers' `baseFeePerGas` before,
/// at and after `Eip1559Block` and `Gas2500xBlock`, and wrong-tier headers made from them.
pub fn base_fee(rpc: &mut Rpc, seed: u64) {
    let spec = Network::Apothem.spec();
    let (e1559, g2500) = (at(spec.eip1559), at(spec.gas2500x));
    let mut rng = Lcg(seed);
    let mut nums: Vec<u64> = vec![];
    for b in [e1559, g2500] {
        nums.extend([b - 10_000, b - 2, b - 1, b, b + 1, b + 2, b + 10_000]);
    }
    nums.extend((0..6).map(|_| rng.between(1, e1559)));
    nums.extend((0..6).map(|_| rng.between(e1559 + 2, g2500 - 2)));
    nums.extend((0..6).map(|_| rng.between(g2500 + 2, HEAD)));
    nums.sort_unstable();
    nums.dedup();
    // parents are needed for the negative vectors
    let all: Vec<u64> = nums
        .iter()
        .flat_map(|n| [*n - 1, *n])
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut hdr = std::collections::BTreeMap::new();
    for chunk in all.chunks(50) {
        let r = many(
            rpc,
            chunk
                .iter()
                .map(|n| ("eth_getBlockByNumber", json!([hx(*n), false])))
                .collect(),
        );
        for h in r.into_iter().filter(|h| h.is_object()) {
            hdr.insert(hex_u64(&h["number"]), h);
        }
    }
    let lines: Vec<Value> = nums
        .iter()
        .filter_map(|n| hdr.get(n))
        .map(|h| {
            json!({
        "network": "apothem", "number": hex_u64(&h["number"]), "hash": h["hash"],
        "baseFeePerGas": h.get("baseFeePerGas").cloned().unwrap_or(Value::Null)})
        })
        .collect();
    write_lines("base_fee.jsonl", &lines);

    // Negative vectors. A signed field changes the seal: only the base-fee rule is tested.
    let gwei = |g: u64| U256::from(g) * U256::from(1_000_000_000u64);
    let tier1 = gwei(12) + U256::from(500_000_000u64);
    let tier2 = tier1 * U256::from(50);
    let rlp = |h: &Header| format!("0x{}", alloy_primitives::hex::encode(alloy_rlp::encode(h)));
    let mut neg = vec![];
    let mut add = |n: u64, rule: &str, recipe: &str, expect: &str, bf: Option<U256>| {
        let (Some(h), Some(p)) = (hdr.get(&n), hdr.get(&(n - 1))) else {
            return;
        };
        let (mut hd, pd) = (header_of(h).unwrap(), header_of(p).unwrap());
        assert_eq!(
            format!("{:#x}", hd.hash()),
            s(&h["hash"]),
            "header {n} does not re-encode to the archive hash"
        );
        let src = format!("apothem-{n}-{}", &s(&h["hash"])[2..10]);
        hd.base_fee = bf;
        neg.push(json!({
            "id": format!("{src}.{rule}"), "network": "apothem", "class": "fork", "number": n,
            "hash": format!("{:#x}", hd.hash()), "header": rlp(&hd), "parent": rlp(&pd),
            "mutation": {"rule": rule, "stage": "header", "recipe": recipe, "expect": expect, "source": src, "activate": null}}));
    };
    let (w, u, m) = (
        "BaseFeeError::Wrong",
        "BaseFeeError::Unexpected",
        "BaseFeeError::Missing",
    );
    for d in [0, 1, 2] {
        add(
            g2500 + d,
            "base_fee_wrong_tier",
            "baseFeePerGas set to the previous tier (12.5 gwei) on a block of the Gas2500x tier",
            w,
            Some(tier1),
        );
    }
    add(
        e1559,
        "base_fee_wrong_tier",
        "baseFeePerGas set to the Gas2500x tier (625 gwei) before Gas2500xBlock",
        w,
        Some(tier2),
    );
    add(
        g2500 - 1,
        "base_fee_wrong_tier",
        "baseFeePerGas set to the Gas2500x tier (625 gwei) on the last block of the old tier",
        w,
        Some(tier2),
    );
    add(
        g2500 + 10_000,
        "base_fee_off_by_one_wei",
        "baseFeePerGas plus one wei",
        w,
        Some(tier2 + U256::from(1)),
    );
    add(
        e1559 + 10_000,
        "base_fee_off_by_one_wei",
        "baseFeePerGas minus one wei",
        w,
        Some(tier1 - U256::from(1)),
    );
    add(
        e1559 - 1,
        "base_fee_before_fork",
        "baseFeePerGas set (12.5 gwei) on the block before Eip1559Block",
        u,
        Some(tier1),
    );
    add(
        e1559 - 10_000,
        "base_fee_before_fork",
        "baseFeePerGas set (12.5 gwei) long before Eip1559Block",
        u,
        Some(tier1),
    );
    add(
        e1559,
        "base_fee_missing",
        "baseFeePerGas removed from the first Eip1559Block header",
        m,
        None,
    );
    add(
        g2500,
        "base_fee_missing",
        "baseFeePerGas removed from the Gas2500xBlock header",
        m,
        None,
    );
    write_lines("base_fee_negative.jsonl", &neg);
}

/// `cases/oog-0x88-*.json`: a call to 0x88 that runs out of gas, from the archive's own
/// `debug_traceCall` at the block before `londonBlock` (so a zero gas price is legal): gas used from
/// the `callTracer`, the prestate and the state change from the `prestateTracer`. It is not a mined
/// transaction: no mined call to 0x88 that ran out of gas was found.
pub fn oog(rpc: &mut Rpc) {
    use crate::case::{AcctIn, BlockIn, ReceiptIn, TxIn};
    let spec = Network::Apothem.spec();
    // Before `londonBlock` the block env has no base fee, so a zero gas price is legal.
    let n = at(spec.london) - 1;
    let gas = 30_000u64;
    let from = "0x00000000000000000000000000000000000000f1";
    let data = selector("getCandidates()");
    let p = json!({"from": from, "to": "0x0000000000000000000000000000000000000088", "data": data,
        "gas": hx(gas), "gasPrice": "0x0"});
    let r = many(
        rpc,
        vec![
            ("eth_getBlockByNumber", json!([hx(n), false])),
            (
                "debug_traceCall",
                json!([p, hx(n), {"tracer": "callTracer"}]),
            ),
            (
                "debug_traceCall",
                json!([p, hx(n), {"tracer": "prestateTracer", "tracerConfig": {"diffMode": false}}]),
            ),
            (
                "debug_traceCall",
                json!([p, hx(n), {"tracer": "prestateTracer", "tracerConfig": {"diffMode": true}}]),
            ),
        ],
    );
    let acct = |v: &Value| AcctIn {
        balance: v["balance"].as_str().map(str::to_string),
        nonce: v["nonce"].as_u64(),
        code: v["code"].as_str().map(str::to_string),
        storage: v["storage"]
            .as_object()
            .map(|m| m.iter().map(|(k, x)| (k.clone(), s(x))).collect())
            .unwrap_or_default(),
    };
    let accts = |v: &Value| {
        v.as_object()
            .map(|m| m.iter().map(|(k, x)| (k.to_lowercase(), acct(x))).collect())
            .unwrap_or_default()
    };
    let (blk, call) = (&r[0], &r[1]);
    assert!(
        call["error"]
            .as_str()
            .is_some_and(|e| e.contains("out of gas")),
        "the call did not run out of gas: {call}"
    );
    let c = Case {
        name: format!("oog-0x88-{n}"),
        coupling: Some("a call to 0x88 that runs out of gas charges all of its gas".into()),
        network: "apothem".into(),
        block: BlockIn {
            number: n,
            timestamp: hex_u64(&blk["timestamp"]),
            miner: format!("{:#x}", author(blk).unwrap()),
            gas_limit: hex_u64(&blk["gasLimit"]),
            difficulty: s(&blk["difficulty"]),
            base_fee: None,
        },
        owner: None,
        tx: TxIn {
            hash: format!("{:#x}", B256::ZERO),
            index: 0,
            ty: 0,
            from: from.into(),
            to: Some("0x0000000000000000000000000000000000000088".into()),
            input: data,
            value: "0x0".into(),
            gas,
            nonce: 0,
            gas_price: "0x0".into(),
            max_priority_fee: None,
            access_list: vec![],
        },
        receipt: ReceiptIn {
            status: 0,
            gas_used: hex_u64(&call["gasUsed"]),
            contract_address: None,
            logs: vec![],
        },
        pre: accts(&r[2]),
        diff_pre: accts(&r[3]["pre"]),
        diff_post: accts(&r[3]["post"]),
        hashes: Default::default(),
    };
    write_case("oog", &c);
    eprintln!(
        "oog: gasUsed {} of {gas}, error {}",
        c.receipt.gas_used, call["error"]
    );
}
