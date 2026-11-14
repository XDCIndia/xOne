//! Sequential replay on the runner's own state, from genesis or from a pivot state. Block N is
//! executed on the persisted state of block N-1 (`StateProvider::commit_block`), its outcome is
//! compared with the canonical header, and the run stops at the first difference. The expected
//! values are the archive's header fields; everything compared against them is computed here.
//! V1 blocks go through the V1 snapshot and checkpoint rules, V2 blocks through the V2 header
//! verifier with the epoch-switch lists derived from the runner's own gap snapshots and penalty
//! engine, the V2 and tiered rewards and the 3-chain commit rule.

use crate::{
    cache::Block,
    chain::{Chain, Pushed},
};
use alloy_primitives::{Address, Bytes, B256, U256};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf};
use xone_chainspec::{ChainSpec, ForkBlock, Network};
use xone_consensus::{
    chain::SignTx,
    v1::{checkpoint, snapshot::Snapshot},
    v2::{epoch_switch, gap_snapshot},
};
use xone_executor::{execute_block, tx, BlockOutcome, ExecCtx};
use xone_primitives::WideBalance;
use xone_state::{
    system_contracts::validator_0x88 as v88, trie::flat_root, Account, FlatRead, FlatWrite,
    MemFlat, StateDiff, StateProvider,
};

/// Changesets kept for unwind; a replay never unwinds, so the window is small.
const RETENTION: u64 = 64;

pub struct Opts {
    pub to: u64,
    /// Where checkpoints of the persisted state are written (and read, with `resume`).
    pub state_dir: Option<PathBuf>,
    pub every: u64,
    pub resume: bool,
    /// Keep what each block's execution produced in `Report::executed` (the RPC parity sweep).
    pub keep: bool,
    /// Print a line every this many blocks (0: none).
    pub progress: u64,
    /// Test mutant: read each gap snapshot from the gap block this many blocks earlier (0: the rule).
    pub gap_offset: u64,
    /// Write the state of this block, as the runner holds it after verifying it, to this file (the
    /// format of the pivot cache, `pivot::start` reads it back).
    pub dump_state: Option<(u64, PathBuf)>,
}

/// What one executed block produced: its receipts, state diff, deployed code and the V1 snapshot
/// after it.
#[derive(Debug, Clone)]
pub struct Executed {
    pub number: u64,
    pub hash: B256,
    pub receipts: Vec<Bytes>,
    pub diff: StateDiff,
    pub codes: BTreeMap<B256, Bytes>,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone)]
pub struct Mismatch {
    pub field: &'static str,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug)]
pub struct Divergence {
    pub block: u64,
    pub mismatches: Vec<Mismatch>,
    /// The executor's error, when the block did not execute.
    pub error: Option<String>,
    /// What our execution wrote, for the archive diagnosis.
    pub diff: Option<StateDiff>,
}

#[derive(Debug, Default)]
pub struct Report {
    /// Highest block that matched.
    pub last_ok: u64,
    /// Blocks executed and compared in this run.
    pub blocks: u64,
    /// Block hashes compared with the archive's own `eth_getBlockByNumber` hash.
    pub archive_hash_checks: u64,
    /// Checkpoint headers whose masternode list was derived from the snapshot and compared with
    /// `extraData`.
    pub aux_checked: u64,
    /// Checkpoints with penalties, whose list needs the penalty derivation (not done here).
    pub aux_skipped: u64,
    pub resumed_from: Option<u64>,
    /// V2 blocks executed, the epoch switches among them (their `Validators` and `Penalties` were
    /// derived by the runner and found equal, or the block would have been refused), the gap
    /// snapshots built from state, and the 3-chain commit state at the end of the run.
    pub v2_blocks: u64,
    pub v2_switches: u64,
    /// The first of those switches, by block number.
    pub first_switches: Vec<u64>,
    pub gap_snapshots: u64,
    pub committed: Option<(u64, u64)>,
    pub committed_count: u64,
    /// Wall time of the first and the last 1,000 blocks of the run, in seconds, to see whether the
    /// per-block cost grows.
    pub first_1000_secs: f64,
    pub last_1000_secs: f64,
    /// The accounts whose storage a block of the run wrote (what a pruned state must keep).
    pub storage_written: std::collections::BTreeSet<Address>,
    pub divergence: Option<Divergence>,
    /// With `Opts::keep`, every block executed in this run, in order.
    pub executed: Vec<Executed>,
}

fn hex(v: impl std::fmt::LowerHex) -> String {
    format!("{v:#x}")
}

/// Genesis state, code and header hash of `net`, after the genesis hash and root checks.
pub fn genesis(
    net: Network,
    spec: &ChainSpec,
) -> Result<(MemFlat, BTreeMap<B256, Bytes>, xone_primitives::Header), String> {
    let g = net.genesis().map_err(|e| e.to_string())?;
    let mut flat = MemFlat::recording();
    let mut codes = BTreeMap::new();
    for (a, acc) in &g.alloc {
        let code_hash = if acc.code.is_empty() {
            alloy_primitives::KECCAK256_EMPTY
        } else {
            let h = alloy_primitives::keccak256(&acc.code);
            codes.insert(h, acc.code.clone());
            h
        };
        flat.set_account(
            *a,
            Some(Account {
                nonce: acc.nonce,
                balance: WideBalance::from(acc.balance),
                code_hash,
                ..Default::default()
            }),
        );
        for (k, v) in &acc.storage {
            flat.set_slot(*a, *k, U256::from_be_bytes(v.0));
        }
    }
    let header = g.to_header(spec).map_err(|e| e.to_string())?;
    if header.hash() != spec.genesis_hash || flat_root(&flat) != spec.genesis_state_root {
        return Err("genesis hash or state root differs from the chain spec".into());
    }
    Ok((flat, codes, header))
}

#[derive(Serialize, Deserialize)]
struct Ckpt {
    number: u64,
    hash: B256,
    accounts: Vec<(Address, u64, Bytes, B256)>,
    storage: Vec<(Address, B256, U256)>,
    codes: BTreeMap<B256, Bytes>,
    signers: Vec<Address>,
    recents: BTreeMap<u64, Address>,
    signs: BTreeMap<u64, Vec<(Address, B256, bool)>>,
    /// V2 gap snapshots (the epoch switch after a resume reads the one of its own gap block).
    #[serde(default)]
    gaps: BTreeMap<u64, Vec<Address>>,
}

fn ckpt_path(dir: &std::path::Path, n: u64) -> PathBuf {
    dir.join(format!("ckpt-{n:08}.json.gz"))
}

fn save(
    dir: &std::path::Path,
    p: &StateProvider<MemFlat>,
    chain: &Chain,
    snap: &Snapshot,
) -> Result<(), String> {
    if chain.base != 0 || !snap.votes.is_empty() || !snap.tally.is_empty() {
        return Ok(()); // a snapshot with votes in flight is not saved: the checkpoint would lose them
    }
    let (number, hash) = p.head();
    let flat = p.flat();
    let accounts: Vec<_> = flat.accounts().collect();
    // A key written by hash only has no address or slot key to save (#251): such a checkpoint
    // would load as another state. Refuse it.
    let unnamed_accounts = flat.hashed_accounts().count() - accounts.len();
    let unnamed_slots: usize = flat
        .hashed_accounts()
        .map(|(ha, _)| flat.hashed_slots(ha).count())
        .sum::<usize>()
        - accounts
            .iter()
            .map(|(a, _)| flat.slots(*a).count())
            .sum::<usize>();
    if unnamed_accounts + unnamed_slots != 0 {
        return Err(format!(
            "checkpoint {number}: {unnamed_accounts} accounts and {unnamed_slots} slots have no known address or key"
        ));
    }
    let c = Ckpt {
        number,
        hash,
        storage: accounts
            .iter()
            .flat_map(|(a, _)| flat.slots(*a).map(|(k, v)| (*a, k, v)))
            .collect(),
        accounts: accounts
            .into_iter()
            .map(|(a, x)| {
                (
                    a,
                    x.nonce,
                    Bytes::from(x.balance.to_be_bytes()),
                    x.code_hash,
                )
            })
            .collect(),
        codes: chain.codes.clone(),
        signers: snap.get_signers(),
        recents: snap.recents.clone(),
        signs: chain
            .blocks
            .iter()
            .filter(|b| !b.signs.is_empty())
            .map(|b| {
                (
                    b.header.number,
                    b.signs.iter().map(|s| (s.sender, s.signed, s.ok)).collect(),
                )
            })
            .collect(),
        gaps: chain
            .gaps
            .iter()
            .map(|(n, g)| (*n, g.candidates.clone()))
            .collect(),
    };
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("ckpt.tmp");
    let z = GzEncoder::new(
        fs::File::create(&tmp).map_err(|e| e.to_string())?,
        Compression::fast(),
    );
    serde_json::to_writer(z, &c).map_err(|e| e.to_string())?;
    fs::rename(&tmp, ckpt_path(dir, number)).map_err(|e| e.to_string())
}

/// The flat state a checkpoint holds.
fn flat_of(c: &Ckpt) -> MemFlat {
    let mut f = MemFlat::recording();
    for (a, nonce, bal, code_hash) in &c.accounts {
        f.set_account(
            *a,
            Some(Account {
                nonce: *nonce,
                balance: WideBalance::from_be_bytes(bal),
                code_hash: *code_hash,
                ..Default::default()
            }),
        );
    }
    for (a, k, v) in &c.storage {
        f.set_slot(*a, *k, *v);
    }
    f
}

/// The number of the checkpoint a resumed run to `to` starts from.
pub fn resume_point(dir: &std::path::Path, to: u64) -> Option<u64> {
    load(dir, to).map(|c| c.number)
}

/// The newest checkpoint of `dir` at or below `to`.
fn load(dir: &std::path::Path, to: u64) -> Option<Ckpt> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("ckpt-"))
        .map(|e| e.path())
        .collect();
    files.sort();
    files.iter().rev().find_map(|f| {
        let c: Ckpt = serde_json::from_reader(GzDecoder::new(fs::File::open(f).ok()?)).ok()?;
        (c.number <= to).then_some(c)
    })
}

/// geth `updateM1At` (`core/blockchain.go:3963-3990`) and `UpdateMasternodesV1`
/// (`consensus/XDPoS/xdpos.go:2135-2150`, `:2156-2174`): at a V1 gap block the snapshot's signers
/// become the cap-sorted candidates of 0x88 in that block's state.
fn reseed(
    spec: &ChainSpec,
    snap: &mut Snapshot,
    state: &impl xone_state::StateRead,
    n: u64,
) -> Result<(), String> {
    let x = &spec.xdpos;
    let at_gap = x.gap != 0 && x.gap <= x.epoch && n % x.epoch == x.epoch - x.gap;
    let v1 = x.v2.as_ref().is_none_or(|v| n <= v.switch_block);
    if !(at_gap && v1) {
        return Ok(());
    }
    let cands: Vec<_> = v88::candidates(state)
        .into_iter()
        .map(|a| (a, v88::candidate_cap(state, a)))
        .collect();
    let tip = match spec.tip_increase_masternodes {
        ForkBlock::At(b) => Some(b),
        _ => None,
    };
    let max = checkpoint::max_masternodes(
        n,
        tip,
        x.v2.as_ref().map(|v| v.switch_block),
        x.max_masternodes_v2,
    );
    snap.reseed(checkpoint::select_candidates(&cands, max).map_err(|e| e.to_string())?);
    Ok(())
}

/// The accounts of `flat` above 2^256.
fn wide_of(flat: &MemFlat) -> BTreeMap<Address, WideBalance> {
    flat.accounts()
        .filter(|(_, a)| a.balance.is_wide())
        .map(|(a, x)| (a, x.balance))
        .collect()
}

fn compare(b: &Block, o: &BlockOutcome) -> Vec<Mismatch> {
    let h = &b.header;
    let mut m = vec![];
    let mut check = |field, expected: String, actual: String| {
        if expected != actual {
            m.push(Mismatch {
                field,
                expected,
                actual,
            });
        }
    };
    check("gasUsed", h.gas_used.to_string(), o.gas_used.to_string());
    check("receiptsRoot", hex(h.receipt_root), hex(o.receipt_root));
    check("logsBloom", hex(h.bloom), hex(o.bloom));
    check("transactionsRoot", hex(h.tx_root), hex(o.tx_root));
    check("stateRoot", hex(h.state_root), hex(o.state_root));
    if let Some(want) = h.block_access_list_hash {
        check(
            "blockAccessListHash",
            hex(want),
            o.bal_hash.map_or("none".into(), hex),
        );
    }
    if let Some(a) = b.archive_hash {
        check("hash", hex(a), hex(o.hash));
    }
    m
}

/// The state a run starts from: the post-state of block `number`, and the code of its contracts.
pub struct Start {
    pub number: u64,
    pub flat: MemFlat,
    pub codes: BTreeMap<B256, Bytes>,
    /// Accounts whose slots are not in `flat`, with their storage root (a pruned state, see
    /// `StateProvider::with_opaque`). Empty for a full state.
    pub opaque: BTreeMap<Address, B256>,
    /// Gap snapshots of V2 gap blocks below `number`, which the rules above it read (`pivot::gap_pools`).
    pub gaps: BTreeMap<u64, xone_consensus::v2::gap_snapshot::SnapshotV2>,
}

/// The sign transactions of a block as its transactions show them (senders recovered): what the
/// reward and penalty engines count. Used for blocks that are stored but not executed.
fn signs_of(spec: &ChainSpec, b: &Block) -> Result<Vec<SignTx>, String> {
    let mut out = vec![];
    for (i, raw) in b.body.transactions.iter().enumerate() {
        let t = tx::decode(raw, spec.chain_id).map_err(|e| format!("block {} tx {i}: {e}", b.n))?;
        if let Some(signed) = xone_consensus::chain::signed_hash(t.to, &t.data) {
            out.push(SignTx {
                sender: t.from,
                signed,
                ok: true,
            });
        }
    }
    Ok(out)
}

/// Replay blocks `1..=opts.to` of `blocks` (block 0 is the genesis block) on `net`'s genesis
/// state. `spec` is the chain spec the rules are read from: the network's, or a mutated copy.
pub fn run(
    net: Network,
    spec: &ChainSpec,
    blocks: &[Block],
    opts: &Opts,
) -> Result<Report, String> {
    let (flat, codes, g) = genesis(net, spec)?;
    if blocks.first().map(|b| b.header.hash()) != Some(g.hash()) {
        return Err("block 0 of the cache is not the genesis block of the chain spec".into());
    }
    run_from(
        spec,
        Start {
            number: 0,
            flat,
            codes,
            opaque: BTreeMap::new(),
            gaps: BTreeMap::new(),
        },
        blocks,
        opts,
    )
}

/// Replay `blocks` above `start.number` up to `opts.to`. `blocks` is contiguous and starts at or
/// below `start.number`: the blocks up to it are history (headers, sign transactions) that the
/// rules read, stored without being executed. The state of `start` is trusted by the caller.
pub fn run_from(
    spec: &ChainSpec,
    start: Start,
    blocks: &[Block],
    opts: &Opts,
) -> Result<Report, String> {
    let mut report = Report::default();
    let start_opaque = start.opaque.clone();
    let first = blocks.first().ok_or("no blocks")?.n;
    let at = |n: u64| -> Result<&Block, String> {
        blocks
            .get(
                usize::try_from(
                    n.checked_sub(first)
                        .ok_or(format!("block {n} is below the cache"))?,
                )
                .map_err(|e| e.to_string())?,
            )
            .filter(|b| b.n == n)
            .ok_or(format!("block {n} is not in the cache"))
    };
    let x = &spec.xdpos;
    let switch = x.v2.as_ref().map_or(u64::MAX, |v| v.switch_block);
    let genesis_snap = |h: &xone_primitives::Header| {
        checkpoint::masternodes_of(&h.extra_data)
            .map(|m| Snapshot::new(h.number, h.hash(), m))
            .map_err(|e| e.to_string())
    };
    let resumed = match (&opts.state_dir, opts.resume, start.number) {
        (Some(d), true, 0) => load(d, opts.to),
        _ => None,
    };
    let (mut chain, mut provider, mut snap, mut tip);
    if let Some(c) = resumed {
        // The headers up to the checkpoint were verified when it was written; their hashes and
        // seals are recomputed, their sign transactions come from the checkpoint.
        chain = Chain::new(spec, 0);
        chain.gap_offset = opts.gap_offset;
        for n in 0..=c.number {
            let b = at(n)?;
            let signs = c.signs.get(&n).map_or(vec![], |l| {
                l.iter()
                    .map(|(sender, signed, ok)| SignTx {
                        sender: *sender,
                        signed: *signed,
                        ok: *ok,
                    })
                    .collect()
            });
            chain.push(Pushed {
                hash: b.header.hash(),
                header: b.header.clone(),
                signs,
            })?;
        }
        if chain.get(c.number).map(|s| s.hash) != Some(c.hash) {
            return Err("the checkpoint is of another chain than the cache".into());
        }
        let f = flat_of(&c);
        // The loaded state must be the state of the checkpoint block (#251): a dropped or
        // misread field shows here, not as a mismatch at the next block.
        let (root, want) = (flat_root(&f), at(c.number)?.header.state_root);
        if root != want {
            return Err(format!(
                "checkpoint {}: the loaded state root {root} differs from the header's {want}",
                c.number
            ));
        }
        chain.codes = c.codes;
        chain.gaps = c
            .gaps
            .into_iter()
            .map(|(number, candidates)| (number, gap_snapshot::SnapshotV2 { number, candidates }))
            .collect();
        chain.wide = wide_of(&f);
        snap = Snapshot::new(c.number, c.hash, c.signers);
        snap.recents = c.recents;
        provider = StateProvider::new(f, c.number, c.hash, RETENTION);
        tip = c.number;
        report.resumed_from = Some(c.number);
    } else {
        chain = Chain::new(spec, first);
        chain.gap_offset = opts.gap_offset;
        for n in first..=start.number {
            let b = at(n)?;
            chain.push(Pushed {
                hash: b.header.hash(),
                header: b.header.clone(),
                signs: signs_of(spec, b)?,
            })?;
        }
        let head = chain.get(start.number).ok_or("no start block")?;
        let hash = head.hash;
        snap = if start.number > switch {
            Snapshot::new(0, B256::ZERO, vec![]) // not read from the V2 switch block on
        } else if start.number.is_multiple_of(x.epoch) {
            genesis_snap(&head.header)?
        } else {
            return Err(
                "a pivot in the V1 era must be a checkpoint (a multiple of the epoch)".into(),
            );
        };
        chain.codes = start.codes;
        chain.gaps = start.gaps;
        chain.wide = wide_of(&start.flat);
        provider =
            StateProvider::with_opaque(start.flat, start.number, hash, RETENTION, &start.opaque);
        tip = start.number;
    }
    report.last_ok = tip;

    let t0 = std::time::Instant::now();
    let mut t_1000 = t0;
    while tip < opts.to {
        let n = tip + 1;
        let b = at(n)?;
        let is_v2 = chain.is_v2(n);
        let parent = chain
            .get(n - 1)
            .ok_or(format!("block {} is not stored", n - 1))?;
        let verify = if is_v2 {
            chain.v2_context(&b.header)?
        } else {
            chain.v1_context(&b.header, &snap)?
        };
        let derive = |q: &epoch_switch::SwitchQuery| {
            let s = chain.snapshot_for(q.number)?;
            epoch_switch::expected(spec, &chain, q, &s)
        };
        let result = {
            let view = provider.at(parent.hash).map_err(|e| e.to_string())?;
            execute_block(
                spec,
                &view,
                &b.header,
                &b.body,
                &ExecCtx {
                    parent: &parent.header,
                    verify: &verify,
                    env: &chain,
                    derive: is_v2.then_some(&derive),
                },
            )
        };
        let parent_hash = parent.hash;
        let o = match result {
            Ok(o) => o,
            Err(e) => {
                report.divergence = Some(Divergence {
                    block: n,
                    mismatches: vec![],
                    error: Some(e.to_string()),
                    diff: None,
                });
                return Ok(report);
            }
        };
        let mut mismatches = compare(b, &o);
        // Consensus auxiliary: the masternode list of a V1 checkpoint is the snapshot's signers
        // (ascending, `Prepare`, `consensus/XDPoS/xdpos.go:1439-1476`) minus the penalised: the
        // header's own `Penalties` and those of the checkpoints 1 to 4 epochs back (`:1468-1472`).
        if !is_v2 && n.is_multiple_of(x.epoch) {
            let mut previous = BTreeMap::new();
            for i in 1..=checkpoint::LIMIT_PENALTY_EPOCH {
                if let Some(k) = n.checked_sub(i * x.epoch).filter(|k| *k > 0) {
                    let h = chain
                        .get(k)
                        .ok_or(format!("no checkpoint {k} for the penalties"))?;
                    previous.insert(k, checkpoint::addresses(&h.header.penalties));
                }
            }
            let removed = checkpoint::checkpoint_penalties(
                n,
                x.epoch,
                &checkpoint::addresses(&b.header.penalties),
                &previous,
            )
            .map_err(|e| e.to_string())?;
            let derived: std::collections::BTreeSet<_> = snap
                .get_signers()
                .into_iter()
                .filter(|a| !removed.contains(a))
                .collect();
            let derived: Vec<_> = derived.into_iter().collect();
            let listed =
                checkpoint::masternodes_of(&b.header.extra_data).map_err(|e| e.to_string())?;
            if listed != derived {
                mismatches.push(Mismatch {
                    field: "extraData",
                    expected: format!("{listed:?}"),
                    actual: format!("{derived:?}"),
                });
            }
            report.aux_checked += 1;
        }
        let body_error = o.body_check.as_ref().err().map(|e| e.to_string());
        if !mismatches.is_empty() || body_error.is_some() {
            report.divergence = Some(Divergence {
                block: n,
                mismatches,
                error: body_error,
                diff: Some(o.diff),
            });
            return Ok(report);
        }
        report.archive_hash_checks += u64::from(b.archive_hash.is_some());
        provider
            .commit_block(o.hash, parent_hash, &o.diff)
            .map_err(|e| e.to_string())?;
        if !is_v2 {
            snap = snap
                .apply(std::slice::from_ref(&b.header), x.epoch)
                .map_err(|e| e.to_string())?;
            reseed(
                spec,
                &mut snap,
                &provider.at(o.hash).map_err(|e| e.to_string())?,
                n,
            )?;
        }
        // geth `Finalize` (`consensus/XDPoS/xdpos.go:1521-1540`): a V2-era gap block stores the pool
        // read from its own state, for the epoch switch that follows.
        if chain.is_v2_gap_block(n) {
            let pool = gap_snapshot::build(&provider.at(o.hash).map_err(|e| e.to_string())?, n)
                .map_err(|e| format!("gap block {n}: {e}"))?;
            chain.gaps.insert(n, pool);
            report.gap_snapshots += 1;
        }
        report
            .storage_written
            .extend(o.diff.storage.keys().map(|(a, _)| *a));
        if opts.keep {
            report.executed.push(Executed {
                number: n,
                hash: o.hash,
                receipts: o.receipt_encodings.clone(),
                diff: o.diff.clone(),
                codes: o.codes.clone(),
                snapshot: snap.clone(),
            });
        }
        chain.codes.extend(o.codes);
        for (a, acc) in &o.diff.accounts {
            match acc {
                Some(x) if x.balance.is_wide() => chain.wide.insert(*a, x.balance.clone()),
                _ => chain.wide.remove(a),
            };
        }
        chain.push(Pushed {
            hash: o.hash,
            header: b.header.clone(),
            signs: o.signs,
        })?;
        if is_v2 {
            report.v2_blocks += 1;
            if chain.get(n).is_some_and(|s| s.epoch_switch) {
                report.v2_switches += 1;
                if report.first_switches.len() < 8 {
                    report.first_switches.push(n);
                }
            }
        }
        tip = n;
        report.last_ok = n;
        report.blocks += 1;
        if let Some((at, path)) = &opts.dump_state {
            if *at == n {
                crate::pivot::dump(path, n, provider.flat(), &chain.codes, &start_opaque)?;
            }
        }
        if report.blocks == 1000 {
            report.first_1000_secs = t0.elapsed().as_secs_f64();
            t_1000 = std::time::Instant::now();
        }
        if opts.every == 0 && chain.blocks.len() > 30_000 {
            chain.prune(n - 20_000);
        }
        if let (Some(d), true) = (
            &opts.state_dir,
            opts.every != 0 && (n % opts.every == 0 || n == opts.to),
        ) {
            save(d, &provider, &chain, &snap)?;
        }
        if opts.progress != 0 && n % opts.progress == 0 {
            eprintln!(
                "block {n}: {} blocks, {} V2 switches, {:.0}s",
                report.blocks,
                report.v2_switches,
                t0.elapsed().as_secs_f64()
            );
        }
    }
    if report.blocks > 1000 {
        report.last_1000_secs =
            t_1000.elapsed().as_secs_f64() * 1000.0 / (report.blocks - 1000) as f64;
    }
    report.committed = chain.committed.map(|l| (l.number, l.round));
    report.committed_count = chain.committed_count;
    Ok(report)
}

#[cfg(test)]
mod ckpt_tests {
    use super::*;
    use alloy_primitives::address;

    /// #251: accounts and slots a block creates are written by hash; the checkpoint still names
    /// them, and the loaded state has the root of the saved one.
    #[test]
    fn a_checkpoint_loads_as_the_state_it_saved() {
        let spec = xone_chainspec::net2040();
        let (old, new) = (
            address!("00000000000000000000000000000000000000aa"),
            address!("00000000000000000000000000000000000000bb"),
        );
        let mut f = MemFlat::recording();
        f.set_account(
            old,
            Some(Account {
                nonce: 1,
                ..Default::default()
            }),
        );
        let mut p = StateProvider::new(f, 0, B256::repeat_byte(1), RETENTION);
        let diff = StateDiff {
            accounts: BTreeMap::from([(
                new,
                Some(Account {
                    nonce: 7,
                    balance: WideBalance::from(U256::from(5u64)),
                    ..Default::default()
                }),
            )]),
            storage: BTreeMap::from([
                ((new, B256::with_last_byte(1)), U256::from(9u64)),
                ((old, B256::with_last_byte(2)), U256::from(3u64)),
            ]),
        };
        p.commit_block(B256::repeat_byte(2), B256::repeat_byte(1), &diff)
            .unwrap();
        let dir = std::env::temp_dir().join(format!("xone-ckpt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let chain = Chain::new(&spec, 0);
        save(&dir, &p, &chain, &Snapshot::new(0, B256::ZERO, vec![])).unwrap();
        let c = load(&dir, 10).unwrap();
        assert_eq!(c.number, 1);
        assert_eq!(c.accounts.len(), 2);
        assert_eq!(c.storage.len(), 2);
        assert_eq!(flat_root(&flat_of(&c)), p.root());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A flat state that does not know an address refuses to be saved.
    #[test]
    fn a_state_with_unnamed_keys_is_not_saved() {
        let spec = xone_chainspec::net2040();
        let mut f = MemFlat::recording();
        f.set_hashed_account(
            B256::repeat_byte(3),
            Some(Account {
                nonce: 1,
                ..Default::default()
            }),
        );
        let p = StateProvider::new(f, 0, B256::repeat_byte(1), RETENTION);
        let dir = std::env::temp_dir().join(format!("xone-ckpt-u-{}", std::process::id()));
        let e = save(
            &dir,
            &p,
            &Chain::new(&spec, 0),
            &Snapshot::new(0, B256::ZERO, vec![]),
        )
        .unwrap_err();
        assert!(e.contains("1 accounts and 0 slots"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }
}
