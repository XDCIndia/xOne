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
        assert!(out(&o).starts_with("xOne/v"), "{args:?}: {}", out(&o));
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
