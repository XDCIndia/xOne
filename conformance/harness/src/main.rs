//! ```text
//! xone-replay fetch --to N [--cache PATH]                       archive blocks 0..=N into the cache (needs ssh xdc02)
//! xone-replay run --network apothem --to N [--source PATH] ...   sequential replay on own state; exit 1 on the first mismatch
//! xone-replay diagnostic-prestate [ARGS]                         oracle-prestate replay via xone-sweep (DIAGNOSTIC, never a gate)
//! ```
#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode, time::Instant};
use xone_chainspec::Network;
use xone_harness::{
    cache::{self, decode},
    diag::diagnose,
    replay::{run, Opts},
};
use xone_sweep::fetch::Rpc;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[derive(Subcommand)]
enum Cmd {
    /// Fetch blocks 0..=to from the archive into the cache (gitignored).
    Fetch {
        #[arg(long)]
        to: u64,
        #[arg(long, default_value_os_t = root().join("cache/apothem.jsonl"))]
        cache: PathBuf,
    },
    /// DIAGNOSTIC: per-transaction replay on archive prestate (`xone-sweep run`). It proves nothing
    /// about accumulated state and never marks a gate; use it to isolate a divergence of `run`.
    DiagnosticPrestate {
        /// Arguments of `xone-sweep run` (filter, mutant).
        args: Vec<String>,
    },
    /// Replay blocks 1..=to from the genesis state on xOne's own state.
    Run {
        #[arg(long, default_value = "apothem")]
        network: String,
        #[arg(long)]
        to: u64,
        /// Block cache, plain or `.gz` (default: the cache written by `fetch`).
        #[arg(long, default_value_os_t = root().join("cache/apothem.jsonl"))]
        source: PathBuf,
        /// Where checkpoints of the persisted state go.
        #[arg(long, default_value_os_t = root().join("state/apothem"))]
        state_dir: PathBuf,
        #[arg(long, default_value_t = 1000)]
        checkpoint_every: u64,
        /// Continue from the newest checkpoint at or below `--to`.
        #[arg(long)]
        resume: bool,
        /// Where the divergence record is written.
        #[arg(long, default_value_os_t = root().join("../divergences"))]
        divergences: PathBuf,
        /// Query the archive (eth_getProof, prestateTracer) to narrow a state mismatch.
        #[arg(long)]
        diagnose: bool,
    },
}

fn main() -> ExitCode {
    match real(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("xone-replay: {e}");
            ExitCode::from(2)
        }
    }
}

fn real(cli: Cli) -> Result<ExitCode, String> {
    match cli.cmd {
        Cmd::Fetch { to, cache } => {
            let n = cache::fetch(&cache, 0, to)?;
            println!("fetched {n} blocks, cache holds 0..={to}");
            Ok(ExitCode::SUCCESS)
        }
        Cmd::DiagnosticPrestate { args } => {
            eprintln!(
                "DIAGNOSTIC: oracle prestate, transactions on archive state. This is not a gate."
            );
            let sweep = std::env::current_exe()
                .map_err(|e| e.to_string())?
                .with_file_name("xone-sweep");
            let status = std::process::Command::new(sweep)
                .arg("run")
                .args(args)
                .status()
                .map_err(|e| e.to_string())?;
            Ok(if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Cmd::Run {
            network,
            to,
            source,
            state_dir,
            checkpoint_every,
            resume,
            divergences,
            diagnose: diag,
        } => {
            let net: Network = network.parse().map_err(|e| format!("{e}"))?;
            if net != Network::Apothem {
                return Err("only apothem has a V1 context here".into());
            }
            let t0 = Instant::now();
            let blocks = cache::read(&source)?
                .iter()
                .take(usize::try_from(to).map_err(|e| e.to_string())? + 1)
                .map(decode)
                .collect::<Result<Vec<_>, _>>()?;
            let spec = net.spec();
            let opts = Opts {
                to,
                state_dir: Some(state_dir),
                every: checkpoint_every,
                resume,
            };
            let r = run(net, &spec, &blocks, &opts)?;
            let secs = t0.elapsed().as_secs_f64();
            match r.divergence {
                None => {
                    println!(
                        "{network} 0..={to}: 0 mismatches, {} blocks executed this run{} ({} archive hash checks, {} checkpoint lists derived, {} with penalties skipped), {secs:.1}s",
                        r.blocks,
                        r.resumed_from.map_or(String::new(), |n| format!(", resumed from {n}")),
                        r.archive_hash_checks,
                        r.aux_checked,
                        r.aux_skipped
                    );
                    Ok(ExitCode::SUCCESS)
                }
                Some(d) => {
                    let mut rec = format!(
                        "# {network} block {}\n\nlast matching block: {}\n\n",
                        d.block, r.last_ok
                    );
                    for m in &d.mismatches {
                        rec += &format!(
                            "- {}: expected (canonical) {}, actual (replay) {}\n",
                            m.field, m.expected, m.actual
                        );
                    }
                    if let Some(e) = &d.error {
                        rec += &format!("- execution or validation error: {e}\n");
                    }
                    if let (true, Some(diff)) = (diag, &d.diff) {
                        if d.mismatches.iter().any(|m| m.field == "stateRoot") {
                            rec += &format!(
                                "\n## state diagnosis (archive eth_getProof, prestateTracer)\n\n{}",
                                diagnose(&mut Rpc::default(), d.block, diff)?
                            );
                        }
                    }
                    std::fs::create_dir_all(&divergences).map_err(|e| e.to_string())?;
                    let path = divergences.join(format!("{network}-{}.md", d.block));
                    std::fs::write(&path, &rec).map_err(|e| e.to_string())?;
                    eprintln!("{rec}\nrecord: {}", path.display());
                    Ok(ExitCode::from(1))
                }
            }
        }
    }
}
