//! ```text
//! xone-replay fetch [--network apothem|net2040] [--to N] [--cache PATH]   archive blocks 0..=N into the cache (needs ssh xdc02)
//! xone-replay run --network apothem|net2040 --to N [--source PATH] ...   sequential replay on own state; exit 1 on the first mismatch
//! xone-replay pivot --network apothem --block N --to M ...      DIAGNOSTIC-PIVOT: archive state of block N, own-state replay N+1..=M
//! xone-replay diagnostic-prestate [ARGS]                         oracle-prestate replay via xone-sweep (DIAGNOSTIC, never a gate)
//! ```
#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode, time::Instant};
use xone_chainspec::Network;
use xone_harness::{
    cache::{self, decode},
    diag::diagnose,
    pivot,
    replay::{run, run_from, Divergence, Opts, Report},
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
        /// `apothem` or `net2040`: the archive to read.
        #[arg(long, default_value = "apothem")]
        network: String,
        /// First block of a new cache file; an existing cache continues after its last line.
        #[arg(long, default_value_t = 0)]
        from: u64,
        /// Last block; with `--network net2040` the default is the archive's current head.
        #[arg(long)]
        to: Option<u64>,
        /// Default: `cache/<network>.jsonl`.
        #[arg(long)]
        cache: Option<PathBuf>,
    },
    /// DIAGNOSTIC: per-transaction replay on archive prestate (`xone-sweep run`). It proves nothing
    /// about accumulated state and never marks a gate; use it to isolate a divergence of `run`.
    DiagnosticPrestate {
        /// Arguments of `xone-sweep run` (filter, mutant).
        args: Vec<String>,
    },
    /// DIAGNOSTIC-PIVOT: start from the archive's post-state of `--block` (refused unless it
    /// rebuilds to the header's stateRoot) and replay the blocks above it on xOne's own state. It
    /// proves execution and consensus rules from an archive-sourced state, not bootstrap trust.
    Pivot {
        #[arg(long, default_value = "apothem")]
        network: String,
        #[arg(long)]
        block: u64,
        #[arg(long)]
        to: u64,
        /// Blocks before the pivot that the rules read (headers, sign transactions); they are
        /// stored, not executed.
        #[arg(long, default_value_t = 8100)]
        history: u64,
        /// Block cache that holds `block - history ..= to + 1` (`fetch --from`).
        #[arg(long)]
        source: PathBuf,
        /// Where the downloaded state of the pivot is cached.
        #[arg(long)]
        state_cache: Option<PathBuf>,
        #[arg(long, default_value_os_t = root().join("../divergences"))]
        divergences: PathBuf,
        #[arg(long, default_value_t = 0)]
        progress: u64,
        /// Write the state of `--dump-block`, as the runner holds it after verifying it, to this file.
        #[arg(long, requires = "dump_block")]
        dump_state: Option<PathBuf>,
        #[arg(long)]
        dump_block: Option<u64>,
        /// A deliberately wrong rule that the run must catch: `tier-off` (the tiered reward off),
        /// `v2-reward-off` (reward 0 from V2), `gap-wrong` (the gap snapshot of the epoch before).
        #[arg(long)]
        mutant: Option<String>,
        /// Write the accounts whose storage the run wrote, one address per line.
        #[arg(long)]
        written_out: Option<PathBuf>,
        /// A file of contract addresses, one per line, that the window reaches through internal
        /// calls (`debug_traceBlockByNumber` callTracer) and that must be held in full.
        #[arg(long)]
        callees_file: Option<PathBuf>,
    },
    /// Prune a pivot state dump for a committed test: keep the slots of `--keep` accounts only.
    Prune {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        to: PathBuf,
        /// A file of addresses, one per line (`pivot --written-out`), plus any given here.
        #[arg(long)]
        keep_file: Option<PathBuf>,
        #[arg(long, value_delimiter = ',')]
        keep: Vec<alloy_primitives::Address>,
    },
    /// Replay blocks 1..=to from the genesis state on xOne's own state.
    Run {
        #[arg(long, default_value = "apothem")]
        network: String,
        /// Must be 0: a run starts at the genesis state, or at a checkpoint with `--resume`.
        #[arg(long, default_value_t = 0)]
        from: u64,
        #[arg(long)]
        to: u64,
        /// Block cache, plain or `.gz` (default: the cache written by `fetch`).
        #[arg(long)]
        source: Option<PathBuf>,
        /// Where checkpoints of the persisted state go (default `state/<network>`).
        #[arg(long)]
        state_dir: Option<PathBuf>,
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
        /// Print a progress line every this many blocks.
        #[arg(long, default_value_t = 0)]
        progress: u64,
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
        Cmd::Fetch {
            network,
            from,
            to,
            cache,
        } => {
            let net: Network = network.parse().map_err(|e| format!("{e}"))?;
            let cache = cache.unwrap_or_else(|| root().join(format!("cache/{network}.jsonl")));
            let to = match to {
                Some(t) => t,
                None => {
                    let head = Rpc::for_network(net)?
                        .batch(vec![("eth_blockNumber", serde_json::json!([]))])?;
                    xone_sweep::fetch::hex_u64(&head[0]["result"])
                }
            };
            let n = cache::fetch(net, &cache, from, to)?;
            println!("fetched {n} blocks, cache holds {from}..={to}");
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Prune {
            from,
            to,
            keep_file,
            mut keep,
        } => {
            if let Some(f) = keep_file {
                for l in std::fs::read_to_string(f)
                    .map_err(|e| e.to_string())?
                    .lines()
                {
                    keep.push(l.trim().parse().map_err(|e| format!("{l}: {e}"))?);
                }
            }
            let (slots, opaque) = pivot::prune(&from, &to, &keep)?;
            println!(
                "kept {slots} slots of {} accounts, {opaque} accounts with their storage root only",
                keep.len()
            );
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
            from,
            to,
            source,
            state_dir,
            checkpoint_every,
            resume,
            divergences,
            diagnose: diag,
            progress,
        } => {
            let net: Network = network.parse().map_err(|e| format!("{e}"))?;
            if from != 0 {
                return Err("--from must be 0: use --resume to continue from a checkpoint".into());
            }
            let source = source.unwrap_or_else(|| root().join(format!("cache/{network}.jsonl")));
            let state_dir = state_dir.unwrap_or_else(|| root().join(format!("state/{network}")));
            let t0 = Instant::now();
            let blocks = cache::read_limit(&source, usize::try_from(to).map_err(|e| e.to_string())? + 1)?
                .iter()
                .map(decode)
                .collect::<Result<Vec<_>, _>>()?;
            let spec = net.spec();
            let opts = Opts {
                to,
                state_dir: Some(state_dir),
                every: checkpoint_every,
                resume,
                keep: false,
                progress,
                gap_offset: 0,
                dump_state: None,
            };
            let r = run(net, &spec, &blocks, &opts)?;
            finish(&network, "", to, r, t0, diag, &divergences)
        }
        Cmd::Pivot {
            network,
            block,
            to,
            history,
            source,
            state_cache,
            divergences,
            progress,
            dump_state,
            dump_block,
            mutant,
            written_out,
            callees_file,
        } => {
            let net: Network = network.parse().map_err(|e| format!("{e}"))?;
            if net != Network::Apothem {
                return Err("only apothem has an archive here".into());
            }
            let mut spec = net.spec();
            let mut gap_offset = 0;
            match mutant.as_deref() {
                None => {}
                Some("tier-off") => {
                    spec.tip_fixed_reward = xone_chainspec::ForkBlock::Never;
                    spec.tip_proportional_reward = xone_chainspec::ForkBlock::Never;
                }
                Some("v2-reward-off") => spec.xdpos.reward = 0,
                Some("gap-wrong") => gap_offset = spec.xdpos.epoch,
                Some(m) => return Err(format!("unknown mutant {m}")),
            }
            eprintln!(
                "DIAGNOSTIC-PIVOT: the state of block {block} is archive-sourced; this proves execution and consensus rules, not bootstrap trust (XA-09)"
            );
            let t0 = Instant::now();
            let lo = block.saturating_sub(history);
            let blocks = cache::read(&source)?
                .iter()
                .filter(|l| l.n >= lo && l.n <= to + 1)
                .map(decode)
                .collect::<Result<Vec<_>, _>>()?;
            let (first, last) = (blocks.first().map(|b| b.n), blocks.last().map(|b| b.n));
            if first != Some(lo) || last != Some(to + 1) {
                return Err(format!(
                    "the cache holds {first:?}..={last:?}, the pivot needs {lo}..={}",
                    to + 1
                ));
            }
            let at = |n: u64| &blocks[(n - lo) as usize];
            let (head, next) = (at(block), at(block + 1));
            let path = state_cache
                .unwrap_or_else(|| root().join(format!("cache/pivot-{network}-{block}.json.gz")));
            // contracts the window calls must not be held as a storage root only: ask for them in full
            let mut callees = std::collections::BTreeSet::new();
            for b in blocks.iter().filter(|b| b.n > block && b.n <= to) {
                for raw in &b.body.transactions {
                    if let Ok(t) = xone_executor::tx::decode(raw, spec.chain_id) {
                        callees.extend(t.to);
                    }
                }
            }
            if let Some(f) = callees_file {
                for l in std::fs::read_to_string(f)
                    .map_err(|e| e.to_string())?
                    .lines()
                {
                    callees.insert(l.trim().parse().map_err(|e| format!("{l}: {e}"))?);
                }
            }
            let filled = pivot::fill(&path, next.header.hash(), &callees)?;
            if filled > 0 {
                eprintln!("{filled} contracts of the window downloaded in full");
            }
            let mut start = pivot::start(
                net,
                &path,
                block,
                head.header.state_root,
                next.header.hash(),
            )?;
            // the V2 gap blocks below the pivot whose snapshots the rules above it read
            let (epoch, gap) = (spec.xdpos.epoch, spec.xdpos.gap);
            let switch = spec.xdpos.v2.as_ref().map_or(u64::MAX, |v| v.switch_block);
            let gaps: Vec<u64> = (1..=4u64)
                .filter_map(|i| (block - block % epoch).checked_sub(i * epoch - epoch + gap))
                .filter(|g| *g >= switch && *g < block)
                .collect();
            start.gaps = pivot::gap_pools(&mut Rpc::default(), &gaps, |g| {
                blocks
                    .iter()
                    .find(|b| b.n == g + 1)
                    .map(|b| b.header.hash())
                    .ok_or(format!("block {} is not in the cache", g + 1))
            })?;
            let t_state = t0.elapsed().as_secs_f64();
            eprintln!("state ready in {t_state:.0}s");
            let opts = Opts {
                to,
                state_dir: None,
                every: 0,
                resume: false,
                keep: false,
                progress,
                gap_offset,
                dump_state: dump_state.zip(dump_block).map(|(p, b)| (b, p)),
            };
            let t_run = Instant::now();
            let r = run_from(&spec, start, &blocks[..blocks.len() - 1], &opts)?;
            if let Some(p) = written_out {
                let list: Vec<String> = r
                    .storage_written
                    .iter()
                    .map(|a| format!("{a:#x}"))
                    .collect();
                std::fs::write(p, list.join("\n")).map_err(|e| e.to_string())?;
            }
            finish(
                &network,
                &format!("DIAGNOSTIC-PIVOT at {block}, "),
                to,
                r,
                t_run,
                false,
                &divergences,
            )
        }
    }
}

/// Prints the result of a run and writes the divergence record, if any.
fn finish(
    network: &str,
    label: &str,
    to: u64,
    r: Report,
    t0: Instant,
    diag: bool,
    divergences: &std::path::Path,
) -> Result<ExitCode, String> {
    let secs = t0.elapsed().as_secs_f64();
    match r.divergence {
        None => {
            println!(
                "{label}{network} ..={to}: 0 mismatches, {} blocks executed this run{} ({} archive hash checks, {} checkpoint lists derived, {} with penalties skipped), {secs:.1}s",
                r.blocks,
                r.resumed_from.map_or(String::new(), |n| format!(", resumed from {n}")),
                r.archive_hash_checks,
                r.aux_checked,
                r.aux_skipped
            );
            if r.v2_blocks > 0 {
                println!(
                    "{label}V2: {} blocks, {} epoch switches (Validators and Penalties derived by the runner, equal to the header at each), {} gap snapshots from state (first switches at {:?}), 3-chain commit: {} blocks committed, highest {:?} (number, round); per block: first 1,000 blocks {:.3} s total, steady state {:.3} ms",
                    r.v2_blocks, r.v2_switches, r.gap_snapshots, r.first_switches, r.committed_count, r.committed,
                    r.first_1000_secs, r.last_1000_secs
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(d) => record(network, label, &r.last_ok, d, diag, divergences),
    }
}

fn record(
    network: &str,
    label: &str,
    last_ok: &u64,
    d: Divergence,
    diag: bool,
    divergences: &std::path::Path,
) -> Result<ExitCode, String> {
    let mut rec = format!(
        "# {label}{network} block {}\n\nlast matching block: {last_ok}\n\n",
        d.block
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
                diagnose(&mut Rpc::for_network(network.parse().map_err(|e| format!("{e}"))?)?, d.block, diff)?
            );
        }
    }
    std::fs::create_dir_all(divergences).map_err(|e| e.to_string())?;
    let path = divergences.join(format!("{network}-{}.md", d.block));
    std::fs::write(&path, &rec).map_err(|e| e.to_string())?;
    eprintln!("{rec}\nrecord: {}", path.display());
    Ok(ExitCode::from(1))
}
