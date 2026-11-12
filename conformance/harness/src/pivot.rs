//! DIAGNOSTIC-PIVOT: the full post-state of one block, read from the Apothem archive, as the
//! starting state of a sequential replay.
//!
//! The state is archive-sourced: it proves that execution and consensus rules carry on correctly
//! from a state geth produced, not that the state was authenticated (that is XA-09's anchor work).
//! It is accepted only if the root rebuilt from it equals the `stateRoot` of the block header.
//!
//! Sources, all read only: `debug_accountRange` (accounts, 256 per call, without code), `eth_getCode`
//! (one call per distinct code hash), `debug_storageRangeAt` on the next block at transaction 0
//! (the slots of each account with storage, with their preimages), and `eth_getProof` for the
//! accounts that can hold more than 2^256 (`debug_accountRange` prints such a balance modulo
//! 2^256, found by #76; the true balance is in the account leaf).

use crate::replay::Start;
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::Decodable;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::Path};
use xone_chainspec::Network;
use xone_primitives::WideBalance;
use xone_state::{trie::flat_root, Account, FlatWrite, MemFlat};
use xone_sweep::fetch::Rpc;

/// The accounts that are, or ever were, above 2^256 on Apothem (`spikes/wide-balance/REPORT.md`):
/// the genesis whales are added from the chain spec. Their true balance is read from the leaf.
const APOTHEM_WIDE: [&str; 5] = [
    "0xa65010026b83368ca05df6e8b467985d6de3eac5",
    "0x38b20a2594939531373efcd2e6aa54d03fc534be",
    "0xb6c1f9da410d0739efbe934b4c30dfbee2755233",
    "0x6c7ff2b9a79ee2b9113966f67a895c09dca3c759",
    "0x2a5c77b016df1b3b0ae4e79a68f8adf64ee741ba",
];

#[derive(Serialize, Deserialize, Default)]
struct Dump {
    number: u64,
    accounts: Vec<(Address, u64, Bytes, B256, B256)>,
    storage: Vec<(Address, B256, U256)>,
    codes: BTreeMap<B256, Bytes>,
    /// Accounts whose slots were left out of a pruned dump (`prune`), with their storage root.
    #[serde(default)]
    opaque: Vec<(Address, B256)>,
    /// The balances of the possibly wide accounts were read from their leaf (`fix_wide`).
    #[serde(default)]
    wide_from_leaf: bool,
}

fn call(rpc: &mut Rpc, method: &str, params: Value) -> Result<Value, String> {
    let a = rpc.batch(vec![(method, params)])?.remove(0);
    match a.get("result") {
        Some(r) if !r.is_null() => Ok(r.clone()),
        _ => Err(format!("{method}: {}", a["error"])),
    }
}

fn u256_dec(v: &Value) -> Result<U256, String> {
    match v {
        Value::String(s) => U256::from_str_radix(s, 10).map_err(|e| format!("{s}: {e}")),
        Value::Number(n) => Ok(U256::from(n.as_u64().ok_or("balance")?)),
        _ => Err(format!("balance {v}")),
    }
}

fn hex_be(v: &Value) -> Result<Vec<u8>, String> {
    let s = v.as_str().ok_or_else(|| format!("not a string: {v}"))?;
    let s = s.trim_start_matches("0x");
    let s = if s.len() % 2 == 1 {
        format!("0{s}")
    } else {
        s.to_string()
    };
    alloy_primitives::hex::decode(s).map_err(|e| e.to_string())
}

/// The part of the download that is safe to keep: `path` with a suffix.
fn part(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    p.into()
}

/// Accounts, wide balances and code. Kept in `<path>.stage1` so a failed storage download does not
/// repeat the 15 minutes it took.
fn download_accounts(net: Network, rpc: &mut Rpc, number: u64) -> Result<Dump, String> {
    let at = format!("{number:#x}");
    let mut d = Dump {
        number,
        ..Default::default()
    };
    let mut start = B256::ZERO;
    let mut pages = 0u64;
    loop {
        let r = call(
            rpc,
            "debug_accountRange",
            json!([at, format!("{start:#x}"), 256, true, true, true]),
        )?;
        for (k, a) in r["accounts"].as_object().ok_or("accounts")? {
            let addr: Address = a["address"]
                .as_str()
                .unwrap_or(k)
                .parse()
                .map_err(|_| format!("account {k} has no address preimage"))?;
            let nonce = a["nonce"].as_u64().ok_or("nonce")?;
            let balance = u256_dec(&a["balance"])?;
            d.accounts.push((
                addr,
                nonce,
                Bytes::from(balance.to_be_bytes_vec()),
                serde_json::from_value(a["root"].clone()).map_err(|e| e.to_string())?,
                serde_json::from_value(a["codeHash"].clone()).map_err(|e| e.to_string())?,
            ));
        }
        pages += 1;
        if pages.is_multiple_of(100) {
            eprintln!("accounts: {} ({pages} pages)", d.accounts.len());
        }
        match r["next"].as_str() {
            Some(n) if !n.is_empty() => start = n.parse().map_err(|_| format!("next {n}"))?,
            _ => break,
        }
    }
    eprintln!("accounts: {} in {pages} pages", d.accounts.len());
    fix_wide(net, rpc, &mut d)?;
    // code, once per hash
    let empty = keccak256([]);
    let mut first: BTreeMap<B256, Address> = BTreeMap::new();
    for (a, _, _, _, h) in &d.accounts {
        if *h != empty {
            first.entry(*h).or_insert(*a);
        }
    }
    let todo: Vec<_> = first.into_iter().collect();
    for chunk in todo.chunks(50) {
        let calls = chunk
            .iter()
            .map(|(_, a)| ("eth_getCode", json!([format!("{a:#x}"), at])))
            .collect();
        for ((h, _), ans) in chunk.iter().zip(rpc.batch(calls)?) {
            let code = Bytes::from(hex_be(&ans["result"])?);
            if keccak256(&code) != *h {
                return Err(format!("code of hash {h} does not hash to it"));
            }
            d.codes.insert(*h, code);
        }
    }
    eprintln!("codes: {}", d.codes.len());
    Ok(d)
}

/// The true balance of the accounts that can be above 2^256, from the account leaf: the last node
/// of the `eth_getProof` account proof is `[path, rlp(account)]`, whose balance item is the whole
/// integer (the `balance` field of the answer and of `debug_accountRange` is modulo 2^256).
fn fix_wide(net: Network, rpc: &mut Rpc, d: &mut Dump) -> Result<(), String> {
    let at = format!("{:#x}", d.number);
    let mut wide: Vec<Address> = APOTHEM_WIDE.iter().filter_map(|a| a.parse().ok()).collect();
    if net == Network::Apothem {
        if let Ok(g) = net.genesis() {
            wide.extend(
                g.alloc
                    .iter()
                    .filter(|(_, a)| a.balance >= U256::from(1) << 255usize)
                    .map(|(a, _)| *a),
            );
        }
    }
    wide.sort();
    wide.dedup();
    for chunk in wide.chunks(20) {
        let calls = chunk
            .iter()
            .map(|a| ("eth_getProof", json!([format!("{a:#x}"), [], at])))
            .collect();
        for (a, ans) in chunk.iter().zip(rpc.batch(calls)?) {
            let Some(leaf) = ans["result"]["accountProof"]
                .as_array()
                .and_then(|p| p.last())
            else {
                continue; // absent at this block
            };
            let node = hex_be(leaf)?;
            let mut buf: &[u8] = &node;
            let items =
                Vec::<Bytes>::decode(&mut buf).map_err(|e| format!("proof leaf of {a}: {e}"))?;
            let acc: Account = alloy_rlp::decode_exact(items.get(1).ok_or("leaf has no value")?)
                .map_err(|e| format!("account leaf of {a}: {e}"))?;
            if let Some(x) = d.accounts.iter_mut().find(|x| x.0 == *a) {
                if acc.nonce != x.1 || acc.code_hash != x.4 {
                    return Err(format!("leaf of {a} does not match its account entry"));
                }
                x.2 = Bytes::from(acc.balance.to_be_bytes());
            }
        }
    }
    d.wide_from_leaf = true;
    Ok(())
}

/// One line of `<path>.storage`: a slot, or the end of an account.
#[derive(Serialize, Deserialize)]
enum Line {
    Slot(Address, B256, U256),
    /// The next key to ask for after the slots written so far.
    Next(Address, B256),
    Done(Address),
    /// More slots than `MAX_SLOTS`: the account is kept with its storage root only.
    Opaque(Address),
}

/// An account with more slots than this is not downloaded: a handful of NFT contracts hold millions
/// of slots each, hours of requests, and no block of a window need read them. They go into the
/// pruned state (`Dump::opaque`) with the storage root the archive reports; the root of the whole
/// state is still checked against the header, and a block that touched one would not match.
const MAX_SLOTS: usize = 100_000;

/// Storage of every account with a storage root, appended to `<path>.storage` as it arrives: an
/// account is final once its `Done` line is there, a restart skips those. A response that cannot be
/// read (too large, cut) is asked again in halves of the batch down to one account.
fn download_storage(
    rpc: &mut Rpc,
    d: &mut Dump,
    path: &Path,
    next_hash: B256,
    max_slots: usize,
) -> Result<(), String> {
    use std::io::{BufRead, Write};
    let file = part(path, ".storage");
    let mut done: std::collections::BTreeSet<Address> = Default::default();
    let mut next: BTreeMap<Address, B256> = BTreeMap::new();
    let mut counts: BTreeMap<Address, usize> = BTreeMap::new();
    let mut opaque: std::collections::BTreeSet<Address> = Default::default();
    if let Ok(f) = fs::File::open(&file) {
        for l in std::io::BufReader::new(f).lines() {
            let Ok(l) = l else { break };
            match serde_json::from_str::<Line>(&l) {
                Ok(Line::Slot(a, k, v)) => {
                    *counts.entry(a).or_default() += 1;
                    d.storage.push((a, k, v));
                }
                Ok(Line::Opaque(a)) => {
                    opaque.insert(a);
                }
                Ok(Line::Next(a, k)) => {
                    next.insert(a, k);
                }
                Ok(Line::Done(a)) => {
                    done.insert(a);
                }
                Err(_) => break, // a line cut by a crash: everything after it is asked again
            }
        }
    }
    // an account that was not finished continues from its last `Next` key; one with no `Next` and
    // no `Done` starts again, so its slots are dropped
    d.storage
        .retain(|(a, _, _)| !opaque.contains(a) && (done.contains(a) || next.contains_key(a)));
    let empty_root = keccak256([alloy_rlp::EMPTY_STRING_CODE]);
    let mut pending: Vec<(Address, B256)> = d
        .accounts
        .iter()
        .filter(|x| x.3 != empty_root && !done.contains(&x.0) && !opaque.contains(&x.0))
        .map(|x| (x.0, next.get(&x.0).copied().unwrap_or(B256::ZERO)))
        .collect();
    eprintln!(
        "accounts with storage still to fetch: {} ({} done)",
        pending.len(),
        done.len()
    );
    let mut out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .map_err(|e| e.to_string())?;
    let mut size = 20usize;
    let mut calls_done = 0usize;
    let mut round = 0;
    while !pending.is_empty() {
        let mut again = vec![];
        let mut rest: &[(Address, B256)] = &pending;
        while !rest.is_empty() {
            let n = size.min(rest.len());
            let chunk = &rest[..n];
            let calls = chunk
                .iter()
                .map(|(a, k)| {
                    (
                        "debug_storageRangeAt",
                        json!([
                            format!("{next_hash:#x}"),
                            0,
                            format!("{a:#x}"),
                            format!("{k:#x}"),
                            5000
                        ]),
                    )
                })
                .collect();
            let answers = match rpc.batch(calls) {
                Ok(x) => x,
                Err(e) if size > 1 => {
                    eprintln!("batch of {size} failed ({e}), halving");
                    size = (size / 2).max(1);
                    continue;
                }
                Err(e) => return Err(e),
            };
            for ((a, _), ans) in chunk.iter().zip(answers) {
                let r = &ans["result"];
                if r.is_null() {
                    return Err(format!("debug_storageRangeAt {a}: {}", ans["error"]));
                }
                for (hk, s) in r["storage"].as_object().ok_or("storage")? {
                    let key: B256 = serde_json::from_value(s["key"].clone())
                        .map_err(|_| format!("slot hash {hk} of {a} has no preimage"))?;
                    let v = U256::from_be_slice(&hex_be(&s["value"])?);
                    if !v.is_zero() {
                        d.storage.push((*a, key, v));
                        *counts.entry(*a).or_default() += 1;
                        let l = serde_json::to_string(&Line::Slot(*a, key, v))
                            .map_err(|e| e.to_string())?;
                        writeln!(out, "{l}").map_err(|e| e.to_string())?;
                    }
                }
                match r["nextKey"].as_str() {
                    Some(_) if counts.get(a).copied().unwrap_or(0) > max_slots => {
                        let l =
                            serde_json::to_string(&Line::Opaque(*a)).map_err(|e| e.to_string())?;
                        writeln!(out, "{l}").map_err(|e| e.to_string())?;
                        opaque.insert(*a);
                        d.storage.retain(|(x, _, _)| x != a);
                        eprintln!(
                            "{a:#x}: more than {MAX_SLOTS} slots, kept as a storage root only"
                        );
                    }
                    Some(n) => {
                        let n: B256 = n.parse().map_err(|_| format!("nextKey {n}"))?;
                        let l =
                            serde_json::to_string(&Line::Next(*a, n)).map_err(|e| e.to_string())?;
                        writeln!(out, "{l}").map_err(|e| e.to_string())?;
                        again.push((*a, n));
                    }
                    None => {
                        let l =
                            serde_json::to_string(&Line::Done(*a)).map_err(|e| e.to_string())?;
                        writeln!(out, "{l}").map_err(|e| e.to_string())?;
                    }
                }
            }
            rest = &rest[n..];
            calls_done += n;
            if calls_done % 2000 < n {
                eprintln!(
                    "storage: {calls_done} account calls, {} slots",
                    d.storage.len()
                );
            }
        }
        pending = again;
        round += 1;
        if round > 1 {
            eprintln!(
                "storage: round {round}, {} accounts continue",
                pending.len()
            );
        }
    }
    // an account that was cut keeps the root of the accounts table
    d.opaque = d
        .accounts
        .iter()
        .filter(|x| opaque.contains(&x.0))
        .map(|x| (x.0, x.3))
        .collect();
    eprintln!(
        "storage: {} slots, {} accounts kept as a storage root only",
        d.storage.len(),
        d.opaque.len()
    );
    Ok(())
}

fn download(
    net: Network,
    rpc: &mut Rpc,
    path: &Path,
    number: u64,
    next_hash: B256,
) -> Result<Dump, String> {
    let stage1 = part(path, ".stage1");
    let mut d = match read(&stage1).filter(|d| d.number == number) {
        Some(d) => d,
        None => {
            let d = download_accounts(net, rpc, number)?;
            write(&stage1, &d)?;
            d
        }
    };
    download_storage(rpc, &mut d, path, next_hash, MAX_SLOTS)?;
    Ok(d)
}

/// Writes the state of block `number` in the format [`start`] reads.
pub fn dump(
    path: &Path,
    number: u64,
    flat: &MemFlat,
    codes: &BTreeMap<B256, Bytes>,
    opaque: &BTreeMap<Address, B256>,
) -> Result<(), String> {
    let accounts: Vec<_> = flat.accounts().collect();
    let d = Dump {
        number,
        storage: accounts
            .iter()
            .flat_map(|(a, _)| flat.slots(*a).map(|(k, v)| (*a, k, v)))
            .collect(),
        accounts: accounts
            .into_iter()
            .map(|(a, x)| {
                // the storage root field of the flat table is not kept up to date: compute it
                let sr = opaque
                    .get(&a)
                    .copied()
                    .unwrap_or_else(|| xone_state::trie::storage_root(flat.slots(a)));
                (
                    a,
                    x.nonce,
                    Bytes::from(x.balance.to_be_bytes()),
                    sr,
                    x.code_hash,
                )
            })
            .collect(),
        codes: codes.clone(),
        opaque: opaque.iter().map(|(a, r)| (*a, *r)).collect(),
        wide_from_leaf: true,
    };
    write(path, &d)
}

/// A copy of the dump at `from` in which every account that has slots, except those in `keep`, has
/// them left out and its storage root kept (`Dump::opaque`). The root of the whole state is still
/// checked against the header when it is loaded.
pub fn prune(from: &Path, to: &Path, keep: &[Address]) -> Result<(usize, usize), String> {
    let mut d = read(from).ok_or("no dump")?;
    let empty_root = keccak256([alloy_rlp::EMPTY_STRING_CODE]);
    let keep: std::collections::BTreeSet<_> = keep.iter().copied().collect();
    d.storage.retain(|(a, _, _)| keep.contains(a));
    d.opaque = d
        .accounts
        .iter()
        .filter(|x| x.3 != empty_root && !keep.contains(&x.0))
        .map(|x| (x.0, x.3))
        .collect();
    write(to, &d)?;
    Ok((d.storage.len(), d.opaque.len()))
}

fn read(path: &Path) -> Option<Dump> {
    serde_json::from_reader(GzDecoder::new(fs::File::open(path).ok()?)).ok()
}

fn write(path: &Path, d: &Dump) -> Result<(), String> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let z = GzEncoder::new(
        fs::File::create(path).map_err(|e| e.to_string())?,
        Compression::fast(),
    );
    serde_json::to_writer(z, d).map_err(|e| e.to_string())
}

/// The post-state of block `number`, from the cache at `path` or from the archive (and then
/// cached). `state_root` is the header's: a state that does not rebuild to it is refused.
/// `next_hash` is the hash of block `number + 1`, whose parent state `debug_storageRangeAt` reads.
pub fn start(
    net: Network,
    path: &Path,
    number: u64,
    state_root: B256,
    next_hash: B256,
) -> Result<Start, String> {
    let d = match read(path).filter(|d| d.number == number) {
        Some(mut d) => {
            if !d.wide_from_leaf {
                fix_wide(net, &mut Rpc::default(), &mut d)?;
                write(path, &d)?;
            }
            d
        }
        None => {
            let d = download(net, &mut Rpc::default(), path, number, next_hash)?;
            write(path, &d)?;
            for suffix in [".stage1", ".storage"] {
                fs::remove_file(part(path, suffix)).ok();
            }
            d
        }
    };
    let opaque: BTreeMap<Address, B256> = d.opaque.iter().copied().collect();
    let mut flat = MemFlat::recording();
    for (a, nonce, bal, root, code_hash) in &d.accounts {
        flat.set_account(
            *a,
            Some(Account {
                nonce: *nonce,
                balance: WideBalance::from_be_bytes(bal),
                storage_root: *root,
                code_hash: *code_hash,
            }),
        );
    }
    for (a, k, v) in &d.storage {
        flat.set_slot(*a, *k, *v);
    }
    let rebuilt = if opaque.is_empty() {
        flat_root(&flat)
    } else {
        xone_state::mpt::StateTrie::from_flat_pruned(&flat, &opaque).root()
    };
    if rebuilt != state_root {
        // which accounts do not hold what the archive says: storage root of the slots against the
        // account's own root
        let mut bad = vec![];
        for (a, _, _, root, _) in &d.accounts {
            if opaque.contains_key(a) || *root == keccak256([alloy_rlp::EMPTY_STRING_CODE]) {
                continue;
            }
            let have = xone_state::trie::storage_root(flat.slots(*a));
            if have != *root {
                bad.push(format!("{a:#x}: slots give {have}, archive root {root}"));
            }
        }
        eprintln!(
            "{} accounts whose slots do not give the archive's storage root",
            bad.len()
        );
        for b in bad.iter().take(10) {
            eprintln!("  {b}");
        }
        return Err(format!(
            "pivot state of block {number} rebuilds to {rebuilt}, the header's stateRoot is {state_root}: refused"
        ));
    }
    eprintln!(
        "pivot state of block {number}: {} accounts, {} slots, root {rebuilt} equals the header's",
        d.accounts.len(),
        d.storage.len()
    );
    Ok(Start {
        number,
        flat,
        codes: d.codes,
        opaque,
        gaps: BTreeMap::new(),
    })
}

/// A map of the slots of one contract, as a state reader that holds no account.
struct Slots(BTreeMap<B256, U256>);

impl xone_state::StateRead for Slots {
    fn account(&self, _: Address) -> Option<Account> {
        None
    }
    fn storage(&self, _: Address, slot: B256) -> U256 {
        self.0.get(&slot).copied().unwrap_or_default()
    }
}

/// The gap snapshots of the V2 gap blocks below the pivot that the rules above it read (the pool of
/// an epoch is fixed at its gap block, which may precede the pivot): each is built from the 0x88
/// storage of the gap block, read with `debug_storageRangeAt` on the next block (`next_hash(g)` is
/// the hash of block `g + 1`), as the runner builds one from its own state above the pivot.
pub fn gap_pools(
    rpc: &mut Rpc,
    gaps: &[u64],
    next_hash: impl Fn(u64) -> Result<B256, String>,
) -> Result<BTreeMap<u64, xone_consensus::v2::gap_snapshot::SnapshotV2>, String> {
    let mut out = BTreeMap::new();
    for &g in gaps {
        let hash = next_hash(g)?;
        let mut slots = BTreeMap::new();
        let mut from = B256::ZERO;
        loop {
            let r = call(
                rpc,
                "debug_storageRangeAt",
                json!([
                    format!("{hash:#x}"),
                    0,
                    "0x0000000000000000000000000000000000000088",
                    format!("{from:#x}"),
                    2000
                ]),
            )?;
            for (hk, s) in r["storage"].as_object().ok_or("storage")? {
                let key: B256 = serde_json::from_value(s["key"].clone())
                    .map_err(|_| format!("slot hash {hk} of 0x88 has no preimage"))?;
                slots.insert(key, U256::from_be_slice(&hex_be(&s["value"])?));
            }
            match r["nextKey"].as_str() {
                Some(n) => from = n.parse().map_err(|_| format!("nextKey {n}"))?,
                None => break,
            }
        }
        let pool = xone_consensus::v2::gap_snapshot::build(&Slots(slots), g)
            .map_err(|e| format!("gap block {g}: {e}"))?;
        eprintln!(
            "gap snapshot of block {g}: {} candidates",
            pool.candidates.len()
        );
        out.insert(g, pool);
    }
    Ok(out)
}

/// Downloads the whole storage of the accounts of `want` that the dump at `path` holds as a storage
/// root only (the ones a window calls), and rewrites the dump with them in full.
pub fn fill(
    path: &Path,
    next_hash: B256,
    want: &std::collections::BTreeSet<Address>,
) -> Result<usize, String> {
    let mut d = read(path).ok_or("no dump")?;
    let todo: Vec<Address> = d
        .opaque
        .iter()
        .map(|x| x.0)
        .filter(|a| want.contains(a))
        .collect();
    if todo.is_empty() {
        return Ok(0);
    }
    let mut part_d = Dump {
        number: d.number,
        accounts: d
            .accounts
            .iter()
            .filter(|x| todo.contains(&x.0))
            .cloned()
            .collect(),
        ..Default::default()
    };
    let file = part(path, ".fill");
    download_storage(
        &mut Rpc::default(),
        &mut part_d,
        &file,
        next_hash,
        usize::MAX,
    )?;
    fs::remove_file(part(&file, ".storage")).ok();
    d.opaque.retain(|x| !todo.contains(&x.0));
    d.storage.extend(part_d.storage);
    write(path, &d)?;
    Ok(todo.len())
}
