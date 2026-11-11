#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! The real-process kill harness (tests/fault/PLAN.md section 3 and G4): `xone-db-stress` imports the
//! committed Apothem vectors 1 to 1,800 into a real datadir (MDBX and segment files on disk) through
//! the node's `Core`; this test sends it SIGKILL at 20 random points and starts it again on the same
//! datadir. Before each restart the test opens the datadir itself and logs the row `reconcile()` chose.
//! Every reopen must reach one consistent head by rollback, never by wiping, and the last run must
//! finish at block 1,800 with the archive's hash.
//!
//! The datadir is on disk (`XONE_STRESS_DIR`, default the system temp directory), not on tmpfs.

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use alloy_primitives::B256;
use xone_db::{reconcile, segments::OsDir, ManifestState, Recovery};
use xone_fault::cfg;
use xone_sync::{
    chain::resume::resume,
    mdbx::{MdbxStore, OpenError},
    store::Store,
};

const BLOCKS: u64 = 1800;
const KILLS: u64 = 20;

fn rng(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn spawn(dir: &Path) -> (Child, BufReader<std::process::ChildStdout>) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_xone-db-stress"))
        .arg(dir)
        .arg(BLOCKS.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let out = BufReader::new(c.stdout.take().unwrap());
    (c, out)
}

/// What `reconcile` says about the datadir as it was left, and the head it will resume at. `None`
/// for a datadir that does not exist yet.
fn observe(dir: &Path) -> Option<(String, u64)> {
    let root = dir.join("xone");
    if !root.join("chain.mdbx").exists() {
        return None;
    }
    let seg = OsDir::open(&root).unwrap();
    let store = MdbxStore::open(&root.join("chain.mdbx"), seg, cfg(1024, 100_000)).unwrap();
    let scan = store.scan();
    let head = match &scan.manifest {
        ManifestState::Ok(m) => m.head_number,
        _ => u64::MAX,
    };
    let row = match reconcile(scan.generation, &scan.manifest, &scan.tails) {
        Recovery::Ready => "Ready (D11)".to_string(),
        Recovery::Fresh => "Fresh (D1)".to_string(),
        Recovery::Repair(a) => {
            let frames: u64 = a.iter().map(|x| x.dropped_frames).max().unwrap_or(0);
            format!(
                "Repair {} segment(s), {frames} complete frame(s) cut ({})",
                a.len(),
                if frames == 0 {
                    "D12"
                } else if a.iter().any(|x| x.quarantine) {
                    "D14"
                } else {
                    "D13"
                }
            )
        }
        Recovery::Refuse(r) => format!("REFUSE {r:?}"),
    };
    Some((row, head))
}

fn newest_checkpoint(dir: &Path) -> u64 {
    std::fs::read_dir(dir.join("xone/state"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_prefix("ckpt-")?
                .split('.')
                .next()?
                .parse()
                .ok()
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn twenty_sigkills_during_the_apothem_import_never_lose_or_wipe_the_datadir() {
    let base = std::env::var_os("XONE_STRESS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("xone-stress-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut seed = std::env::var("XONE_STRESS_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x1800_u64);
    println!("datadir {} seed {seed}", dir.display());
    let mut rows: Vec<String> = Vec::new();
    let (mut killed_at, mut highest) = (Vec::new(), 0u64);
    let started = Instant::now();
    for k in 1..=KILLS {
        // the state of the datadir as the last kill left it
        let before = observe(&dir);
        if let Some((row, head)) = &before {
            println!("kill {k:>2}: reconcile row before restart: {row}, manifest head {head}");
            assert!(!row.starts_with("REFUSE"), "kill {k}: {row}");
            assert!(!row.starts_with("Fresh"), "kill {k}: the datadir was wiped");
            rows.push(row.clone());
            assert!(*head >= newest_checkpoint(&dir), "kill {k}: head {head} is below a state checkpoint");
            assert!(*head >= highest.min(newest_checkpoint(&dir)));
        }
        let (mut child, out) = spawn(&dir);
        // a second process cannot take the datadir while this one has it (ADR-005 N5)
        let target = 1 + rng(&mut seed) % 60;
        let jitter = Duration::from_micros(rng(&mut seed) % 40_000);
        let mut lines = 0;
        let mut last_head = None;
        let mut finished = false;
        for line in out.lines() {
            let line = line.unwrap();
            if let Some(h) = line.strip_prefix("head ") {
                last_head = h.parse::<u64>().ok();
            }
            if line.starts_with("done") {
                finished = true;
                break;
            }
            lines += 1;
            if lines == 2 {
                let e = OsDir::open(&dir.join("xone"));
                assert!(
                    e.as_ref().is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock),
                    "a second process took the datadir"
                );
            }
            if lines >= target {
                break;
            }
        }
        std::thread::sleep(jitter);
        child.kill().unwrap(); // SIGKILL
        let status = child.wait().unwrap();
        println!(
            "kill {k:>2}: SIGKILL after {lines} lines, last head {:?}, +{} us{}",
            last_head,
            jitter.as_micros(),
            if finished { " (the child had finished)" } else { "" }
        );
        assert!(finished || !status.success());
        if let Some(h) = last_head {
            killed_at.push(h);
            highest = highest.max(h);
        }
        if finished {
            break;
        }
    }
    // the last run is allowed to finish
    let (_, head_before) = observe(&dir).expect("a datadir");
    println!("final restart from manifest head {head_before}");
    let (mut child, out) = spawn(&dir);
    let mut done = None;
    for line in out.lines() {
        let line = line.unwrap();
        if line.starts_with("done") {
            done = Some(line);
            break;
        }
    }
    assert!(child.wait().unwrap().success());
    let done = done.expect("the child never finished");
    println!("{done}  (wall {:?} for {KILLS} kills and the final run)", started.elapsed());
    // the archive's hash of block 1,800 and an independent look at the datadir
    let want: B256 = {
        use std::io::Read;
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../conformance/vectors/replay/apothem-0-1800.jsonl.gz");
        let mut text = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(p).unwrap())
            .read_to_string(&mut text)
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(text.lines().nth(1800).unwrap()).unwrap();
        v["hash"].as_str().unwrap().parse().unwrap()
    };
    assert!(done.ends_with(&format!("{want}")), "{done} vs {want}");
    let root = dir.join("xone");
    let mut store =
        MdbxStore::open(&root.join("chain.mdbx"), OsDir::open(&root).unwrap(), cfg(1024, 100_000)).unwrap();
    resume(&mut store).unwrap();
    assert_eq!(store.head(), (BLOCKS, want));
    for n in 0..=BLOCKS {
        assert!(matches!(store.canonical_block(n), Ok(Some(_))), "block {n}");
    }
    drop(store);
    println!(
        "{} kills, heads at kill {:?}, reconcile rows before restart: {:?}",
        killed_at.len(),
        killed_at,
        rows
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = OpenError::FsyncFailedSameBoot(String::new());
}
