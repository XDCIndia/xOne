//! Process-level tests of the xone binary (XB-19).

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use xone_chainspec::Network;

fn xone(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xone"))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{e}"))
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("xone-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn version_prints_and_never_touches_the_datadir() {
    let dd = scratch("version");
    let dd = dd.to_str().unwrap_or_default();
    for args in [
        vec!["--version"],
        vec!["-version"],
        vec!["-V"],
        vec!["version"],
        vec!["--datadir", dd, "--version"],
        vec!["-datadir", dd, "-version"],
        vec!["--datadir", dd, "--apothem", "version"],
    ] {
        let t = Instant::now();
        let o = xone(&args);
        assert!(
            t.elapsed() < Duration::from_secs(1),
            "{args:?} took {:?}",
            t.elapsed()
        );
        assert!(o.status.success(), "{args:?}");
        assert!(out(&o).starts_with("xOneRS/v"), "{args:?}: {}", out(&o));
        assert!(
            !std::path::Path::new(dd).exists(),
            "{args:?} created the datadir"
        );
    }
}

#[test]
fn unknown_flag_exits_2() {
    for args in [
        vec!["--no-such-flag"],
        vec!["-no.such"],
        vec!["--datadir"],
        vec!["--http=maybe", "dumpconfig"],
    ] {
        assert_eq!(xone(&args).status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn single_and_double_dash_give_the_same_config() {
    let a = xone(&[
        "--datadir",
        "d",
        "--http",
        "--http.port",
        "9001",
        "--maxpeers=7",
        "--nat",
        "extip:1.2.3.4",
        "dumpconfig",
    ]);
    let b = xone(&[
        "-datadir",
        "d",
        "-http",
        "-http.port",
        "9001",
        "-maxpeers=7",
        "-nat",
        "extip:1.2.3.4",
        "dumpconfig",
    ]);
    assert!(
        a.status.success() && out(&a).contains("port = 9001"),
        "{}",
        out(&a)
    );
    assert_eq!(out(&a), out(&b));
    assert!(out(&a).contains("max_peers = 7"));
}

#[test]
fn flags_win_over_the_config_file_and_dump_reloads() {
    let d = scratch("cfg");
    std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
    let file = d.join("c.toml");
    std::fs::write(
        &file,
        "[http]\nport = 9000\nenabled = true\n[p2p]\nmax_peers = 11\n",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let f = file.to_str().unwrap_or_default();
    let o = xone(&["--config", f, "--http.port", "9100", "dumpconfig"]);
    let text = out(&o);
    assert!(
        text.contains("port = 9100") && text.contains("max_peers = 11"),
        "{text}"
    );
    let again = d.join("again.toml");
    std::fs::write(&again, &text).unwrap_or_else(|e| panic!("{e}"));
    let o2 = xone(&["--config", again.to_str().unwrap_or_default(), "dumpconfig"]);
    assert_eq!(text, out(&o2));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn init_prints_the_chainspec_hash_for_the_four_genesis_files() {
    let d = scratch("init");
    std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
    for n in Network::ALL {
        let g = d.join(format!("{}.json", n.name()));
        std::fs::write(&g, n.genesis_json()).unwrap_or_else(|e| panic!("{e}"));
        let dd = d.join(format!("data-{}", n.name()));
        let o = xone(&[
            "--datadir",
            dd.to_str().unwrap_or_default(),
            "init",
            g.to_str().unwrap_or_default(),
        ]);
        assert!(
            o.status.success(),
            "{}: {}",
            n.name(),
            String::from_utf8_lossy(&o.stderr)
        );
        let want = format!(
            "chain id: {}\ngenesis hash: {}\n",
            n.spec().chain_id,
            n.spec().genesis_hash
        );
        assert_eq!(out(&o), want, "{}", n.name());
        // `--override.genesis` start-up path and the genesis-hash subcommand agree
        let h = xone(&["genesis-hash", "--network", n.name()]);
        assert_eq!(out(&h).trim(), n.spec().genesis_hash.to_string());
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn datadir_for_chain_x_refuses_chain_y() {
    let d = scratch("mismatch");
    let g = {
        std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
        let g = d.join("apothem.json");
        std::fs::write(&g, Network::Apothem.genesis_json()).unwrap_or_else(|e| panic!("{e}"));
        g
    };
    let dd = d.join("data");
    let dd = dd.to_str().unwrap_or_default();
    assert!(
        xone(&["--datadir", dd, "init", g.to_str().unwrap_or_default()])
            .status
            .success()
    );
    let o = xone(&["--datadir", dd, "--xdcmainnet"]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("initialised for chain 51") && err.contains("chain 50"),
        "{err}"
    );
    let o = xone(&["--datadir", dd, "--apothem", "--networkid", "50"]);
    assert_eq!(o.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&d);
}

fn kill(pid: u32) {
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
}

#[test]
fn sigterm_stops_the_node_and_ten_in_a_row_are_one_stop() {
    let d = scratch("signal");
    let mut child = Command::new(env!("CARGO_BIN_EXE_xone"))
        .args([
            "--datadir",
            d.to_str().unwrap_or_default(),
            "--apothem",
            "--port",
            "0",
            "--nodiscover",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{e}"));
    let mut lines =
        BufReader::new(child.stdout.take().unwrap_or_else(|| panic!("no stdout"))).lines();
    let first = lines.next().and_then(Result::ok).unwrap_or_default();
    assert!(first.starts_with("started chain 51 genesis 0x"), "{first}");
    let t = Instant::now();
    for _ in 0..10 {
        kill(child.id());
    }
    let status = child.wait().unwrap_or_else(|e| panic!("{e}"));
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert!(status.success(), "{status}");
    let rest: Vec<String> = lines.map_while(Result::ok).collect();
    assert_eq!(
        rest.iter().filter(|l| *l == "stopped").count(),
        1,
        "{rest:?}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// `--log.format json`: every stdout line of a start and a SIGTERM stop is one JSON object.
#[test]
fn json_log_lines_parse() {
    let d = scratch("jsonlog");
    let mut child = Command::new(env!("CARGO_BIN_EXE_xone"))
        .args([
            "--datadir",
            d.to_str().unwrap_or_default(),
            "--apothem",
            "--port",
            "0",
            "--nodiscover",
            "--verbosity",
            "4",
            "--log.format",
            "json",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{e}"));
    let out = child.stdout.take().unwrap_or_else(|| panic!("no stdout"));
    let reader = std::thread::spawn(move || {
        BufReader::new(out)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });
    std::thread::sleep(Duration::from_secs(3));
    kill(child.id());
    assert!(child.wait().unwrap_or_else(|e| panic!("{e}")).success());
    let seen = reader.join().unwrap_or_default();
    assert!(
        seen.first().is_some_and(|l| l.contains("started chain 51")),
        "{seen:?}"
    );
    assert!(
        seen.last()
            .is_some_and(|l| l.contains("\"msg\":\"stopped\"")),
        "{seen:?}"
    );
    for l in &seen {
        let v: serde_json::Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"));
        assert!(
            v["t"].is_u64() && v["lvl"].is_string() && v["msg"].is_string(),
            "{l}"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Blocks 0..=`to` of the committed Apothem vectors, as the blocks of a geth export file.
fn vector_blocks(to: usize) -> Vec<Vec<u8>> {
    use std::io::Read;
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/vectors/replay/apothem-0-1800.jsonl.gz");
    let mut text = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(p).unwrap_or_else(|e| panic!("{e}")))
        .read_to_string(&mut text)
        .unwrap_or_else(|e| panic!("{e}"));
    text.lines()
        .take(to + 1)
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}"));
            let raw: alloy_primitives::Bytes =
                serde_json::from_value(v["raw"].clone()).unwrap_or_else(|e| panic!("{e}"));
            raw.to_vec()
        })
        .collect()
}

#[test]
fn import_and_export_run_the_pipeline_and_write_geth_block_files() {
    let dir = scratch("blockfile");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let blocks = vector_blocks(200);
    let input = dir.join("in.rlp");
    std::fs::write(&input, blocks.concat()).unwrap_or_else(|e| panic!("{e}"));
    let input = input.to_str().unwrap_or_default();

    let o = xone(&["import", input]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(out(&o).starts_with("imported 200 blocks"), "{}", out(&o));
    assert!(out(&o).contains("head 200 0x"), "{}", out(&o));

    // `export <file> from to`: blocks 50..=99 of the file, byte for byte
    let part = dir.join("part.rlp");
    let part = part.to_str().unwrap_or_default();
    let o = xone(&["export", part, "50", "99", "--input", input]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        std::fs::read(part).unwrap_or_else(|e| panic!("{e}")),
        blocks[50..100].concat()
    );
    // without a range: the whole chain, genesis included
    let all = dir.join("all.rlp");
    let all = all.to_str().unwrap_or_default();
    assert!(xone(&["export", all, "--input", input]).status.success());
    assert_eq!(
        std::fs::read(all).unwrap_or_else(|e| panic!("{e}")),
        blocks.concat()
    );
    // a range above the head, and a missing file, exit 1 with a message
    let o = xone(&["export", part, "0", "201", "--input", input]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("larger than head block 200"));
    let o = xone(&["import", "/nonexistent/blocks.rlp"]);
    assert_eq!(o.status.code(), Some(1));
}

const APOTHEM_ANCHOR: &str =
    "83665617=0x2432ce6433608954ef6eaf2feba8f9a2fe90f5a913401eb07d5fa2f1bb499c3f";

#[test]
fn apothem_has_its_required_block_without_a_flag_and_the_operator_replaces_it() {
    let o = out(&xone(&["--apothem", "dumpconfig"]));
    assert!(o.contains(APOTHEM_ANCHOR), "{o}");
    // mainnet starts empty
    let m = out(&xone(&["dumpconfig"]));
    assert!(m.contains("required_blocks = []"), "{m}");
    // an operator entry at the same height replaces it; another height adds
    let h = format!("0x{}", "11".repeat(32));
    let o = out(&xone(&[
        "--apothem",
        "--eth.requiredblocks",
        &format!("83665617={h},7={h}"),
        "dumpconfig",
    ]));
    assert!(
        o.contains(&format!("83665617={h}")) && o.contains(&format!("7={h}")),
        "{o}"
    );
    assert!(!o.contains(APOTHEM_ANCHOR), "{o}");
    // the dump loads back to the same list
    let d = scratch("reqblk");
    std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
    let f = d.join("c.toml");
    std::fs::write(&f, &o).unwrap_or_else(|e| panic!("{e}"));
    let again = out(&xone(&[
        "--config",
        f.to_str().unwrap_or_default(),
        "dumpconfig",
    ]));
    assert_eq!(o, again);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_malformed_required_block_is_refused() {
    for v in ["5", "x=0x00", "5=0x12"] {
        assert_eq!(
            xone(&["--eth.requiredblocks", v, "dumpconfig"])
                .status
                .code(),
            Some(2),
            "{v}"
        );
    }
}

/// #227: `XONE_<FLAG>` and `GETH_<FLAG>` configure the node; a flag wins over both.
#[test]
fn environment_variables_configure_and_flags_win() {
    let run = |env: &[(&str, &str)], args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_xone"));
        c.args(args);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap_or_else(|e| panic!("{e}"))
    };
    let o = run(
        &[("XONE_MAXPEERS", "7"), ("XONE_HTTP_API", "eth,net")],
        &["dumpconfig"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(out(&o).contains("max_peers = 7"), "{}", out(&o));
    assert!(out(&o).contains("\"eth\""), "{}", out(&o));
    let o = run(
        &[("XONE_MAXPEERS", "7")],
        &["--maxpeers", "9", "dumpconfig"],
    );
    assert!(out(&o).contains("max_peers = 9"), "{}", out(&o));
    // a geth env file keeps working, with a warning naming the xone variable
    let o = run(&[("GETH_MAXPEERS", "5")], &["dumpconfig"]);
    assert!(out(&o).contains("max_peers = 5"), "{}", out(&o));
    assert!(String::from_utf8_lossy(&o.stderr).contains("prefer XONE_MAXPEERS"));
    // a bad value exits 2 naming the variable
    let o = run(&[("XONE_MAXPEERS", "many")], &["dumpconfig"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("XONE_MAXPEERS"));
}
