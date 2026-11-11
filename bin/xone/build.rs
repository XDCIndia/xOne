//! Builds the version stamp
//! `xOne/v<semver>-<commit8>[-dirty]-<DDMMYYYY>/<os>-<arch>/rustc<version>`.

use std::{
    env,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn run(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// UTC date as DDMMYYYY (civil-from-days, Howard Hinnant).
fn ddmmyyyy(unix_secs: u64) -> String {
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{day:02}{month:02}{year:04}")
}

fn main() {
    // ponytail: only re-runs when git HEAD/index or this crate's sources change;
    // edits elsewhere in the workspace do not refresh the `-dirty` flag until
    // the next git operation. Release builds come from a clean checkout.
    for p in ["../../.git/HEAD", "../../.git/index", "src", "build.rs"] {
        println!("cargo:rerun-if-changed={p}");
    }

    let commit = run(Command::new("git").args(["rev-parse", "--short=8", "HEAD"]))
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let dirty = commit != "unknown"
        && run(Command::new("git").args(["status", "--porcelain"])).is_some_and(|s| !s.is_empty());

    let secs = env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        });

    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let rustc_version = run(Command::new(rustc).arg("--version"))
        .and_then(|v| v.split_whitespace().nth(1).map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());

    let stamp = format!(
        "xOne/v{}-{}{}-{}/{}-{}/rustc{}",
        env::var("CARGO_PKG_VERSION").unwrap(),
        commit,
        if dirty { "-dirty" } else { "" },
        ddmmyyyy(secs),
        env::var("CARGO_CFG_TARGET_OS").unwrap(),
        env::var("CARGO_CFG_TARGET_ARCH").unwrap(),
        rustc_version,
    );
    println!("cargo:rustc-env=XONE_STAMP={stamp}");
}
