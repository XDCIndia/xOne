//! `rpc-parity`: replays every recorded geth-XDC request (`conformance/vectors/rpc/<network>/`) against
//! the `xone-rpc` handlers over a chain backed by the replay of the blocks the harness holds, and
//! diffs the answer with the recorded one. A row is one of
//!
//! * `equal`: the same JSON (hex case aside);
//! * `differ`: both answered, the JSON differs (the diff is in the report, with JSON pointers);
//! * `not-backed`: the data the answer needs does not exist here (state above the replayed tip, a
//!   network that was not replayed, the executor, the consensus engine for V2). Never skipped silently:
//!   the reason is in the report;
//! * `allowlisted`: the oracle's value is node-local (head, peers, pool, clock), with the reason.
//!
//! Usage: `rpc-parity [--network apothem|net2040|all] [--out DIR] [--tip N]`. Exit 1 on a `differ`.

mod backing;
mod diff;

use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use xone_rpc::{server::Limits, server::Rpc};

/// Methods whose answer is a function of the node, the clock or the live head, not of the chain
/// at a recorded block. Each is recorded once at the head and cannot be replayed (reason required).
const NODE_LOCAL: &[(&str, &str)] = &[
    ("eth_blockNumber", "the live head of the oracle node"),
    ("eth_gasPrice", "the oracle's gas price oracle at its head"),
    ("eth_syncing", "the oracle's sync state"),
    ("eth_feeHistory", "relative to the oracle's live head"),
    ("net_peerCount", "the oracle's peer set"),
    ("net_listening", "the oracle's listener"),
    (
        "web3_clientVersion",
        "the build stamp of the oracle (geth-XDC); xOne returns its own, one stamp function",
    ),
    ("admin_nodeInfo", "the oracle's identity"),
    ("admin_peers", "the oracle's peer set"),
    (
        "eth_getBalance",
        "only the `latest` call is live; block-keyed calls are compared",
    ),
    (
        "eth_getBlockByNumber",
        "only `latest` and the unknown-block probe are live; block-keyed calls are compared",
    ),
    (
        "eth_getBlockSignersByNumber",
        "only the unknown-block probe is live",
    ),
    ("XDPoS_getSnapshot", "only the unknown-block probe is live"),
    ("XDPoS_getSigners", "only the `latest` call is live"),
];

struct Row {
    method: String,
    block: Option<u64>,
    status: &'static str,
    note: String,
    diffs: Vec<String>,
}

fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../vectors/rpc")
}

fn classify(
    rpc: &Rpc,
    line: &Value,
    backing: &backing::Backing,
) -> (&'static str, String, Vec<String>) {
    let req = &line["request"];
    let method = req["method"].as_str().unwrap_or("");
    let want = &line["response"];
    if want.get("omitted").is_some() {
        return (
            "not-backed",
            "the oracle's response was over 1 MiB and is not recorded".into(),
            vec![],
        );
    }
    let class = line["class"].as_str().unwrap_or("");
    // These answer the same on every chain: the registry and a geth decoding quirk.
    let chain_free = ["nosuch_method", "xdpos_getSigners", "eth_getMasternodes"].contains(&method);
    if let Some(why) = backing.network_unbacked().filter(|_| !chain_free) {
        return ("not-backed", why, vec![]);
    }
    if class == "live" {
        // The oracle node does not serve the method at all: nothing to compare.
        let live_comparable = [
            "eth_chainId",
            "net_version",
            "XDPoS_networkInformation",
            "XDPoS_getLatestPoolStatus",
            "nosuch_method",
            "xdpos_getSigners",
        ];
        if !live_comparable.contains(&method) {
            if want["error"]["code"] == -32601 && !method.is_empty() {
                return ("not-backed", format!("the oracle node answers -32601 for {method} (namespace not enabled on the archive)"), vec![]);
            }
            let why = NODE_LOCAL
                .iter()
                .find(|(m, _)| *m == method)
                .map_or("node-local", |(_, r)| *r);
            return ("allowlisted", why.to_owned(), vec![]);
        }
    } else if want["error"]["code"] == -32601 && method != "xdpos_getSigners" {
        return ("not-backed", format!("the oracle node answers -32601 for {method} (namespace not enabled on the archive)"), vec![]);
    }
    if class != "live" && !chain_free {
        if let Some(why) = backing.unbacked(line) {
            return ("not-backed", why, vec![]);
        }
    }
    let got: Value = match rpc
        .handle_bytes(req.to_string().as_bytes())
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        Some(v) => v,
        None => {
            return (
                "differ",
                "no response".into(),
                vec!["/: no response".into()],
            )
        }
    };
    if let Some(m) = got["error"]["message"]
        .as_str()
        .filter(|m| m.starts_with("not backed"))
    {
        return ("not-backed", m.to_owned(), vec![]);
    }
    let mut d = vec![];
    // The `id` is the request's, `jsonrpc` constant: the body of the answer is what is compared.
    for k in ["result", "error"] {
        diff::diff(&want[k], &got[k], &format!("/{k}"), &mut d);
    }
    if d.is_empty() {
        ("equal", String::new(), d)
    } else {
        ("differ", String::new(), d)
    }
}

fn run_network(net: &str, out: &Path, tip: u64) -> Result<(usize, usize), String> {
    let dir = vectors_dir().join(net);
    if !dir.is_dir() {
        return Err(format!(
            "no vectors for {net}: run conformance/vectors/rpc/build/build.py {net}"
        ));
    }
    let backing = backing::Backing::load(net, tip)?;
    let rpc = Rpc::new(backing.backend(), Limits::default());
    let mut files: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    files.sort();
    let mut rows = vec![];
    for f in files
        .iter()
        .filter(|f| f.extension().is_some_and(|e| e == "jsonl"))
    {
        for l in fs::read_to_string(f).map_err(|e| e.to_string())?.lines() {
            let v: Value = serde_json::from_str(l).map_err(|e| format!("{}: {e}", f.display()))?;
            let (status, note, diffs) = classify(&rpc, &v, &backing);
            rows.push(Row {
                method: v["request"]["method"].as_str().unwrap_or("").to_owned(),
                block: v["block"].as_u64(),
                status,
                note,
                diffs,
            });
        }
    }
    let mut table: BTreeMap<&str, ([usize; 4], BTreeSet<u64>)> = BTreeMap::new();
    for r in &rows {
        let e = table.entry(r.method.as_str()).or_default();
        e.0[["equal", "differ", "not-backed", "allowlisted"]
            .iter()
            .position(|s| *s == r.status)
            .unwrap_or(1)] += 1;
        if let (Some(b), "equal" | "differ") = (r.block, r.status) {
            e.1.insert(b);
        }
    }
    println!(
        "\n== {net} (replayed tip {})\n{:<42} {:>6} {:>6} {:>6} {:>10} {:>11}",
        backing.tip, "method", "blocks", "equal", "differ", "not-backed", "allowlisted"
    );
    let mut tot = [0usize; 4];
    for (m, (c, bl)) in &table {
        println!(
            "{m:<42} {:>6} {:>6} {:>6} {:>10} {:>11}",
            bl.len(),
            c[0],
            c[1],
            c[2],
            c[3]
        );
        for i in 0..4 {
            tot[i] += c[i];
        }
    }
    println!(
        "{:<42} {:>6} {:>6} {:>6} {:>10} {:>11}",
        "TOTAL", "", tot[0], tot[1], tot[2], tot[3]
    );
    let report = json!({
        "tool": "xone-rpc-parity",
        "network": net,
        "oracle_pin": fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../PIN")).unwrap_or_default().trim(),
        "replayed_tip": backing.tip,
        "summary": {"equal": tot[0], "differ": tot[1], "not_backed": tot[2], "allowlisted": tot[3]},
        "methods": table.iter().map(|(m, (c, bl))| json!({"method": m, "blocks_compared": bl.len(), "equal": c[0], "differ": c[1], "not_backed": c[2], "allowlisted": c[3]})).collect::<Vec<_>>(),
        "rows": rows.iter().map(|r| json!({"method": r.method, "block": r.block, "status": r.status, "note": r.note, "diffs": r.diffs})).collect::<Vec<_>>(),
    });
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    fs::write(
        out.join(format!("{net}.json")),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    for r in rows.iter().filter(|r| r.status == "differ").take(12) {
        println!(
            "DIFFER {} @{:?}: {}",
            r.method,
            r.block,
            r.diffs
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
    Ok((tot[1], tot[0]))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut net, mut out, mut tip) = (
        "all".to_owned(),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("report"),
        1800u64,
    );
    while let Some(a) = args.next() {
        match a.as_str() {
            "--network" => net = args.next().unwrap_or_default(),
            "--out" => out = args.next().map(PathBuf::from).unwrap_or(out),
            "--tip" => tip = args.next().and_then(|t| t.parse().ok()).unwrap_or(tip),
            _ => {
                eprintln!(
                    "usage: rpc-parity [--network apothem|net2040|all] [--out DIR] [--tip N]"
                );
                std::process::exit(2);
            }
        }
    }
    let nets: Vec<&str> = if net == "all" {
        vec!["apothem", "net2040"]
    } else {
        vec![net.as_str()]
    };
    let mut differ = 0;
    for n in nets {
        match run_network(n, &out, tip) {
            Ok((d, _)) => differ += d,
            Err(e) => {
                eprintln!("{n}: {e}");
                std::process::exit(2);
            }
        }
    }
    std::process::exit(i32::from(differ > 0));
}
