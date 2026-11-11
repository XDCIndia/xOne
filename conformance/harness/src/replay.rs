//! Sequential replay from genesis on the runner's own state. Block N is executed on the persisted
//! state of block N-1 (`StateProvider::commit_block`), its outcome is compared with the canonical
//! header, and the run stops at the first difference. The expected values are the archive's
//! header fields; everything compared against them is computed here.

use crate::{
    cache::Block,
    chain::{Chain, Stored},
};
use alloy_primitives::{Address, Bytes, B256, U256};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf};
use xone_chainspec::{ChainSpec, ForkBlock, Network};
use xone_consensus::{
    chain::SignTx,
    v1::{checkpoint, snapshot::Snapshot},
};
use xone_executor::{execute_block, BlockOutcome, ExecCtx};
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
    pub divergence: Option<Divergence>,
}

fn hex(v: impl std::fmt::LowerHex) -> String {
    format!("{v:#x}")
}

/// Genesis state, code and header hash of `net`, after the genesis hash and root checks.
fn genesis(
    net: Network,
    spec: &ChainSpec,
) -> Result<(MemFlat, BTreeMap<B256, Bytes>, xone_primitives::Header), String> {
    let g = net.genesis().map_err(|e| e.to_string())?;
    let mut flat = MemFlat::default();
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
    if !snap.votes.is_empty() || !snap.tally.is_empty() {
        return Ok(()); // a snapshot with votes in flight is not saved: the checkpoint would lose them
    }
    let (number, hash) = p.head();
    let flat = p.flat();
    let accounts: Vec<_> = flat.accounts().collect();
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
            .enumerate()
            .filter(|(_, b)| !b.signs.is_empty())
            .map(|(i, b)| {
                (
                    i as u64,
                    b.signs.iter().map(|s| (s.sender, s.signed, s.ok)).collect(),
                )
            })
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
    if let Some(a) = b.archive_hash {
        check("hash", hex(a), hex(o.hash));
    }
    m
}

/// Replay blocks `1..=opts.to` of `blocks` (block 0 is the genesis block) on `net`'s genesis
/// state. `spec` is the chain spec the rules are read from: the network's, or a mutated copy.
pub fn run(
    net: Network,
    spec: &ChainSpec,
    blocks: &[Block],
    opts: &Opts,
) -> Result<Report, String> {
    let mut report = Report::default();
    let (flat, codes, g) = genesis(net, spec)?;
    if blocks.first().map(|b| b.header.hash()) != Some(g.hash()) {
        return Err("block 0 of the cache is not the genesis block of the chain spec".into());
    }
    let (mut chain, mut provider, mut snap, mut start);
    let genesis_snap = |h: &xone_primitives::Header| {
        checkpoint::masternodes_of(&h.extra_data)
            .map(|m| Snapshot::new(0, h.hash(), m))
            .map_err(|e| e.to_string())
    };
    let resumed = match (&opts.state_dir, opts.resume) {
        (Some(d), true) => load(d, opts.to),
        _ => None,
    };
    if let Some(c) = resumed {
        // The headers up to the checkpoint were verified when it was written; their hashes and
        // seals are recomputed, their sign transactions come from the checkpoint.
        let mut stored = vec![];
        for b in blocks.iter().take(c.number as usize + 1) {
            let signs = c.signs.get(&b.n).map_or(vec![], |l| {
                l.iter()
                    .map(|(sender, signed, ok)| SignTx {
                        sender: *sender,
                        signed: *signed,
                        ok: *ok,
                    })
                    .collect()
            });
            let miner = if b.n == 0 {
                Address::ZERO
            } else {
                b.header.v1_signer().map_err(|e| e.to_string())?
            };
            stored.push(Stored {
                hash: b.header.hash(),
                header: b.header.clone(),
                miner,
                signs,
            });
        }
        if stored.last().map(|s| s.hash) != Some(c.hash) {
            return Err("the checkpoint is of another chain than the cache".into());
        }
        let mut f = MemFlat::default();
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
        chain = Chain {
            epoch: spec.xdpos.epoch,
            blocks: stored,
            codes: c.codes,
            wide: wide_of(&f),
        };
        snap = Snapshot::new(c.number, c.hash, c.signers);
        snap.recents = c.recents;
        provider = StateProvider::new(f, c.number, c.hash, RETENTION);
        start = c.number;
        report.resumed_from = Some(c.number);
    } else {
        chain = Chain {
            epoch: spec.xdpos.epoch,
            blocks: vec![Stored {
                hash: g.hash(),
                header: g.clone(),
                miner: Address::ZERO,
                signs: vec![],
            }],
            codes,
            wide: wide_of(&flat),
        };
        snap = genesis_snap(&g)?;
        provider = StateProvider::new(flat, 0, g.hash(), RETENTION);
        start = 0;
    }
    report.last_ok = start;

    while start < opts.to {
        let n = start + 1;
        let b = blocks
            .get(n as usize)
            .filter(|b| b.n == n)
            .ok_or(format!("block {n} is not in the cache"))?;
        if spec.xdpos.v2.as_ref().is_some_and(|v| n > v.switch_block) {
            return Err(format!(
                "block {n} is above the V2 switch block: the runner has no V2 context"
            ));
        }
        let parent = &chain.blocks[(n - 1) as usize];
        let verify = chain.v1_context(&b.header, &snap)?;
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
        // (ascending, `Prepare`, `consensus/XDPoS/xdpos.go:1439-1476`) minus the penalised.
        if n % spec.xdpos.epoch == 0 {
            if b.header.penalties.is_empty() {
                let listed =
                    checkpoint::masternodes_of(&b.header.extra_data).map_err(|e| e.to_string())?;
                let derived = snap.get_signers();
                if listed != derived {
                    mismatches.push(Mismatch {
                        field: "extraData",
                        expected: format!("{listed:?}"),
                        actual: format!("{derived:?}"),
                    });
                }
                report.aux_checked += 1;
            } else {
                report.aux_skipped += 1;
            }
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
        snap = snap
            .apply(std::slice::from_ref(&b.header), spec.xdpos.epoch)
            .map_err(|e| e.to_string())?;
        reseed(
            spec,
            &mut snap,
            &provider.at(o.hash).map_err(|e| e.to_string())?,
            n,
        )?;
        chain.codes.extend(o.codes);
        for (a, acc) in &o.diff.accounts {
            match acc {
                Some(x) if x.balance.is_wide() => chain.wide.insert(*a, x.balance.clone()),
                _ => chain.wide.remove(a),
            };
        }
        chain.blocks.push(Stored {
            hash: o.hash,
            header: b.header.clone(),
            miner: b.header.v1_signer().map_err(|e| e.to_string())?,
            signs: o.signs,
        });
        start = n;
        report.last_ok = n;
        report.blocks += 1;
        if let (Some(d), true) = (
            &opts.state_dir,
            opts.every != 0 && (n % opts.every == 0 || n == opts.to),
        ) {
            save(d, &provider, &chain, &snap)?;
        }
    }
    Ok(report)
}
