//! `xone-sweep`: replay real Apothem transactions through xone-evm and compare with the archive.
//!
//! ```text
//! xone-sweep fetch <per-era> [seed]   sample contract-call txs per era into cache/ (needs ssh xdc02)
//! xone-sweep named                    fetch the named regression transactions (needs ssh xdc02)
//! xone-sweep run [filter] [mutant]    replay everything in cache/ offline; exit 1 on a divergence
//! xone-sweep export                   copy the named cases to conformance/vectors/evm/regression/
//! ```

#![forbid(unsafe_code)]

use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};
use xone_sweep::{
    case::{era_of, Case, ERAS},
    compare,
    db::bytes,
    fetch::{author, fetch_case, hex_u64, is_special, Rpc},
    mutants::{general, mutant_of, MUTANTS},
    replay, replay_with, ReplayError,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn cache() -> PathBuf {
    root().join("cache/apothem")
}

fn write_json(p: &Path, v: &impl serde::Serialize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, serde_json::to_vec(v).unwrap()).unwrap();
}

fn log_excluded(block: u64, hash: &str, reason: &str) {
    use std::io::Write;
    let p = cache().join("excluded.jsonl");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
        .unwrap();
    writeln!(
        f,
        "{}",
        json!({"block": block, "hash": hash, "reason": reason})
    )
    .unwrap();
}

struct Lcg(u64);
impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 24) % n
    }
}

fn blocks(rpc: &mut Rpc, nums: &[u64]) -> Vec<Value> {
    let calls = nums
        .iter()
        .map(|n| ("eth_getBlockByNumber", json!([format!("{n:#x}"), true])))
        .collect();
    rpc.batch(calls)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| (!a["result"].is_null()).then(|| a["result"].clone()))
        .collect()
}

/// A transaction that can be a contract call: not special, not a self-send, at least a selector.
fn candidate(tx: &Value) -> bool {
    let to = tx["to"].as_str();
    to.is_some_and(|t| !is_special(t))
        && tx["from"].as_str().map(str::to_lowercase) != to.map(str::to_lowercase)
        && tx["input"].as_str().is_some_and(|i| i.len() >= 10)
}

fn count_cached(era: usize) -> usize {
    let (_, lo, hi) = ERAS[era];
    fs::read_dir(cache())
        .into_iter()
        .flatten()
        .filter_map(|e| {
            e.ok()?
                .file_name()
                .to_str()?
                .strip_suffix(".json")
                .map(String::from)
        })
        .filter_map(|n| n.split('-').next()?.parse::<u64>().ok())
        .filter(|n| (lo..hi).contains(n))
        .count()
}

fn fetch_era(rpc: &mut Rpc, era: usize, want: usize, rng: &mut Lcg) {
    let (name, lo, hi) = ERAS[era];
    let hi = hi.min(85_227_000);
    let mut rounds = 0;
    while count_cached(era) < want && rounds < 400 {
        rounds += 1;
        let nums: Vec<u64> = (0..100).map(|_| lo + rng.below(hi - lo)).collect();
        for b in blocks(rpc, &nums) {
            let n = hex_u64(&b["number"]);
            let txs = b["transactions"].as_array().cloned().unwrap_or_default();
            for t in txs
                .iter()
                .filter(|t| t["to"].as_str().is_some_and(is_special))
            {
                log_excluded(
                    n,
                    t["hash"].as_str().unwrap_or(""),
                    "special tx (sign 0x89, randomize 0x90, XDCx 0x91-0x94): XB-08",
                );
            }
            for t in txs.iter().filter(|t| candidate(t)).take(2) {
                if count_cached(era) >= want {
                    break;
                }
                let idx = hex_u64(&t["transactionIndex"]);
                let path = cache().join(format!("{n}-{idx}.json"));
                if path.exists() {
                    continue;
                }
                match fetch_case(rpc, "apothem", &b, t) {
                    Ok(Ok(c)) => write_json(&path, &c),
                    Ok(Err(why)) => log_excluded(n, t["hash"].as_str().unwrap_or(""), &why),
                    Err(e) => eprintln!("block {n}: {e}"),
                }
            }
        }
        eprintln!(
            "{name}: {} of {want} cached, {} requests",
            count_cached(era),
            rpc.requests
        );
    }
}

/// Opcode `op` is executable code in `code` (data of PUSH instructions skipped).
fn has_opcode(code: &[u8], op: u8) -> bool {
    let mut i = 0;
    while i < code.len() {
        if code[i] == op {
            return true;
        }
        i += 1 + if (0x60..=0x7f).contains(&code[i]) {
            (code[i] - 0x5f) as usize
        } else {
            0
        };
    }
    false
}

enum Kind {
    /// Every contract-call tx of one block.
    Block,
    /// The first contract call at or after the block whose recipient runs this opcode.
    Op(u8),
    /// The first contract creation at or after the block.
    Create,
}

/// Name, what it is a regression for, first block, how to find the transaction.
const NAMED: [(&str, &str, u64, Kind); 11] = [
    ("wedge-85197568", "xOneGo wedge: CHAINID, return-data buffer across frames, CREATE2 creator nonce", 85_197_568, Kind::Block),
    ("ecrecover-high-s-85220370", "xOneGo: ecrecover rejected a high-s signature", 85_220_370, Kind::Block),
    ("zero-length-memory-85220385", "xOneGo: zero-length region with a large offset was charged memory", 85_220_385, Kind::Block),
    ("shr-35.4M", "SHR: Istanbul rules start at TIPXDCXCancellationFee, not at the Istanbul or Constantinople row", 35_400_000, Kind::Op(0x1c)),
    ("sload-58.8M", "SLOAD costs 800 under the Istanbul rules from TIPXDCXCancellationFee", 58_800_000, Kind::Op(0x54)),
    ("delegatecall-62.4M", "DELEGATECALL gas and context before Berlin pricing", 62_400_000, Kind::Op(0xf4)),
    ("staticcall-69.4M", "STATICCALL gas before Berlin pricing", 69_400_000, Kind::Op(0xfa)),
    ("push0-64.0M", "PUSH0 is live from londonBlock while pricing is still Istanbul", 64_000_000, Kind::Op(0x5f)),
    ("create-62.2M", "CREATE in the London marker window: no EIP-3860, no EIP-3541", 62_200_000, Kind::Create),
    ("difficulty-56.4M", "opcode 0x44 is DIFFICULTY before londonBlock", 56_400_000, Kind::Op(0x44)),
    ("prevrandao-68.7M", "opcode 0x44 is PREVRANDAO = keccak256(block number) from londonBlock", 68_700_000, Kind::Op(0x44)),
];

/// Replays clean under the real rules (`None`) or under the mutant.
fn clean(c: &Case, mutant: Option<xone_sweep::mutants::Mutant>) -> bool {
    let r = match mutant {
        Some(m) => replay_with(c, &m),
        None => replay(c),
    };
    r.is_ok_and(|o| compare(c, &o).is_none())
}

fn named_cached(name: &str) -> bool {
    fs::read_dir(cache().join("named"))
        .into_iter()
        .flatten()
        .any(|e| {
            e.ok()
                .and_then(|e| e.file_name().into_string().ok())
                .is_some_and(|f| f.starts_with(&format!("{name}-")))
        })
}

fn fetch_named(rpc: &mut Rpc) {
    for (name, why, start, kind) in NAMED {
        if named_cached(name) {
            continue;
        }
        let mut from = start;
        'scan: for _ in 0..400 {
            let nums: Vec<u64> = if matches!(kind, Kind::Block) {
                vec![start]
            } else {
                (from..from + 100).collect()
            };
            from += 100;
            let bs = blocks(rpc, &nums);
            // Transactions of the window that fit the target, then (for an opcode) the ones whose
            // recipient runs it: all codes in one request.
            let mut picked: Vec<(Value, Value)> = vec![];
            for b in &bs {
                for t in b["transactions"].as_array().into_iter().flatten() {
                    let ok = match &kind {
                        Kind::Block => {
                            (candidate(t) || t["to"].is_null())
                                && !t["to"].as_str().is_some_and(is_special)
                        }
                        Kind::Create => t["to"].is_null(),
                        Kind::Op(_) => candidate(t),
                    };
                    if ok {
                        picked.push((b.clone(), t.clone()));
                    }
                }
            }
            if let Kind::Op(op) = &kind {
                let calls = picked
                    .iter()
                    .map(|(b, t)| {
                        (
                            "eth_getCode",
                            json!([t["to"], format!("{:#x}", hex_u64(&b["number"]))]),
                        )
                    })
                    .collect::<Vec<_>>();
                let codes = if calls.is_empty() {
                    vec![]
                } else {
                    rpc.batch(calls).unwrap_or_default()
                };
                picked = picked
                    .into_iter()
                    .zip(codes)
                    .filter(|(_, c)| has_opcode(&bytes(c["result"].as_str().unwrap_or("0x")), *op))
                    .map(|(p, _)| p)
                    .collect();
            }
            for (b, t) in picked {
                if !matches!(kind, Kind::Block) && named_cached(name) {
                    break;
                }
                let (n, idx) = (hex_u64(&b["number"]), hex_u64(&t["transactionIndex"]));
                match fetch_case(rpc, "apothem", &b, &t) {
                    Ok(Ok(mut c)) => {
                        c.name = format!("{name}-{n}-{idx}");
                        c.coupling = Some(why.to_string());
                        // A tx that passes under the real rules and under the mutant proves nothing.
                        // One that fails under the real rules is a divergence and is kept.
                        if !matches!(kind, Kind::Block)
                            && clean(&c, None)
                            && clean(&c, mutant_of(&c.name))
                        {
                            eprintln!(
                                "{name}: block {n} tx {idx} skipped: the mutant is not detected"
                            );
                            continue;
                        }
                        write_json(
                            &cache().join("named").join(format!("{name}-{n}-{idx}.json")),
                            &c,
                        );
                        eprintln!("{name}: block {n} tx {idx}");
                    }
                    Ok(Err(w)) => eprintln!("{name}: block {n} tx {idx} excluded: {w}"),
                    Err(e) => eprintln!("{name}: {e}"),
                }
            }
            if matches!(kind, Kind::Block) || named_cached(name) {
                break 'scan;
            }
        }
    }
}

fn load_all(filter: &str) -> Vec<Case> {
    let mut out = Vec::new();
    for dir in [cache(), cache().join("named")] {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") && p.to_string_lossy().contains(filter) {
                out.push(serde_json::from_slice(&fs::read(&p).unwrap()).unwrap());
            }
        }
    }
    out.sort_by_key(|c: &Case| (c.block.number, c.tx.index));
    out
}

/// Cases fetched before the coinbase was recovered from the seal carry the RPC `miner`, which is
/// zero for V1 blocks. Replace it with the creator of the block and report any block where a
/// non-zero `miner` differs from it.
fn fix_miner(rpc: &mut Rpc) {
    let mut cases = load_all("");
    let mut nums: Vec<u64> = cases.iter().map(|c| c.block.number).collect();
    nums.sort_unstable();
    nums.dedup();
    let mut authors = BTreeMap::new();
    let mut differs = 0;
    for chunk in nums.chunks(100) {
        let calls = chunk
            .iter()
            .map(|n| ("eth_getBlockByNumber", json!([format!("{n:#x}"), false])))
            .collect();
        for a in rpc.batch(calls).unwrap_or_default() {
            let b = &a["result"];
            if let Ok(who) = author(b) {
                let miner = b["miner"].as_str().unwrap_or("");
                if miner != "0x0000000000000000000000000000000000000000"
                    && miner.to_lowercase() != format!("{who:#x}")
                {
                    differs += 1;
                    eprintln!(
                        "block {}: miner field {miner} is not the creator {who:#x}",
                        hex_u64(&b["number"])
                    );
                }
                authors.insert(hex_u64(&b["number"]), format!("{who:#x}"));
            }
        }
    }
    for c in &mut cases {
        if let Some(a) = authors.get(&c.block.number) {
            c.block.miner = a.clone();
            let named = cache().join("named").join(format!("{}.json", c.name));
            let path = if named.exists() {
                named
            } else {
                cache().join(format!("{}-{}.json", c.block.number, c.tx.index))
            };
            write_json(&path, c);
        }
    }
    eprintln!(
        "{} blocks, {} cases updated, {differs} blocks where miner is not the creator",
        nums.len(),
        cases.len()
    );
}

fn dir_size(p: &Path) -> u64 {
    fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let m = e.metadata().unwrap();
            if m.is_dir() {
                dir_size(&e.path())
            } else {
                m.len()
            }
        })
        .sum()
}

fn run(filter: &str, mutant: Option<xone_sweep::mutants::Mutant>) -> ExitCode {
    let cases = load_all(filter);
    let mut per_era: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    let (mut bad, mut other) = (vec![], vec![]);
    for c in &cases {
        let era = era_of(c.block.number);
        let e = per_era.entry(era).or_default();
        e.0 += 1;
        let result = match mutant {
            Some(m) => replay_with(c, &m),
            None => replay(c),
        };
        match result {
            Ok(o) => {
                if let Some(d) = compare(c, &o) {
                    e.1 += 1;
                    bad.push(format!("{} ({}): {d}", c.name, c.tx.hash));
                }
            }
            Err(ReplayError::MissingBlockHash(n)) => {
                other.push(format!("{}: needs hash of block {n}", c.name))
            }
            Err(ReplayError::Unsupported(m)) => other.push(format!("{}: {m}", c.name)),
            Err(ReplayError::Rejected(m)) => {
                e.1 += 1;
                bad.push(format!("{} ({}): {m}", c.name, c.tx.hash));
            }
        }
    }
    println!("{:<20} {:>10} {:>12}", "era", "txs", "divergences");
    for (i, (name, lo, hi)) in ERAS.iter().enumerate() {
        let (n, d) = per_era.get(&i).copied().unwrap_or_default();
        println!("{name:<20} {n:>10} {d:>12}   blocks {lo}..{hi}");
    }
    println!(
        "total {} txs, {} divergences, {} not covered",
        cases.len(),
        bad.len(),
        other.len()
    );
    for b in &bad {
        println!("DIVERGES {b}");
    }
    for o in &other {
        println!("NOT COVERED {o}");
    }
    let mut ex: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for l in fs::read_to_string(cache().join("excluded.jsonl"))
        .unwrap_or_default()
        .lines()
    {
        let v: Value = serde_json::from_str(l).unwrap();
        if seen.insert(v["hash"].as_str().unwrap_or("").to_string()) {
            *ex.entry(
                v["reason"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(70)
                    .collect(),
            )
            .or_default() += 1;
        }
    }
    for (r, n) in ex {
        println!("EXCLUDED {n:>5}  {r}");
    }
    println!("cache size {} bytes", dir_size(&root().join("cache")));
    if bad.is_empty() && other.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn export() {
    let dst = root().join("../vectors/evm/regression");
    fs::create_dir_all(&dst).unwrap();
    for e in fs::read_dir(cache().join("named")).unwrap().flatten() {
        fs::copy(e.path(), dst.join(e.file_name())).unwrap();
    }
}

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut rpc = Rpc::default();
    match a.first().map(String::as_str) {
        Some("fetch") => {
            let want: usize = a.get(1).and_then(|x| x.parse().ok()).unwrap_or(363);
            let mut rng = Lcg(a.get(2).and_then(|x| x.parse().ok()).unwrap_or(1));
            for era in 0..ERAS.len() {
                fetch_era(&mut rpc, era, want, &mut rng);
            }
            ExitCode::SUCCESS
        }
        Some("named") => {
            fetch_named(&mut rpc);
            ExitCode::SUCCESS
        }
        Some("run") => run(
            a.get(1).map_or("", String::as_str),
            a.get(2).and_then(|n| general(n)),
        ),
        Some("mutants") => {
            let mut ok = true;
            for c in load_all("named") {
                let Some(m) = mutant_of(&c.name) else {
                    continue;
                };
                let (base, mutated) = (clean(&c, None), clean(&c, Some(m)));
                println!(
                    "{:<40} real rules clean: {base:<5} mutant detected: {}",
                    c.name, !mutated
                );
                ok &= base && !mutated;
            }
            for (p, _, why) in MUTANTS {
                println!("mutant {p}: {why}");
            }
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("fix-miner") => {
            fix_miner(&mut rpc);
            ExitCode::SUCCESS
        }
        Some("export") => {
            export();
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: xone-sweep fetch <per-era> [seed] | named | run [filter] | export");
            ExitCode::from(2)
        }
    }
}
