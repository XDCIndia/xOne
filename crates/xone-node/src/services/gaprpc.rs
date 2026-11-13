//! The 0x88 pool of a gap block from an archive RPC (`--snap.gap-rpc`), for a snap sync whose state
//! peers are geth-XDC nodes: geth serves snap/1 only for its recent snapshot layers, so the 0x88
//! state of a gap block below the pivot is not on the wire (`spec/snap/pivot.md`).
//!
//! The source is untrusted. `eth_getProof(0x88, slots, G)` is asked for exactly the slots that
//! `gap_snapshot::build` reads (the candidates length and entries, then the cap of each; geth reads
//! the same in `rebuildGapSnapshotFromState`, `engine_v2/engine.go:1349-1360`), and every proof is
//! verified: the account against the state root of the header of `G`, which the node holds and has
//! linked, each slot against the storage root of that account (`xone_snap::rpcproof`). A forged field
//! or node fails; a source that withholds fails the sync with a typed error, never an empty pool.
//! At most 4 requests a second.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    time::{Duration, Instant},
};

use alloy_primitives::{hex, Address, Bytes, B256, KECCAK256_EMPTY, U256};
use serde_json::{json, Value};
use xone_consensus::v2::gap_snapshot;
use xone_primitives::WideBalance;
use xone_snap::rpcproof::{verify_account, verify_slot};
use xone_state::{system_contracts::validator_0x88, Account, StateRead};

use crate::observe::wire;

/// Slots per request: geth has no cap, public gateways often do.
const BATCH: usize = 64;
/// A request every 250 ms at most.
const GAP: Duration = Duration::from_millis(250);
/// Rounds of "read, ask for what was missing": length, entries, caps, and slack.
const ROUNDS: usize = 16;

/// One `eth_getProof` answer, in request order.
pub struct Reply {
    pub account: Option<Account>,
    pub account_proof: Vec<Bytes>,
    pub slots: Vec<(U256, Vec<Bytes>)>,
}

pub trait ProofFetch {
    fn proof(&mut self, slots: &[B256]) -> Result<Reply, String>;
}

/// `gap_snapshot::build` over the slots fetched so far; a slot not fetched yet is recorded and reads
/// as zero, and the caller asks for it and reads again.
struct Lazy<'a> {
    known: &'a BTreeMap<B256, U256>,
    miss: RefCell<BTreeSet<B256>>,
}

impl StateRead for Lazy<'_> {
    fn account(&self, _: Address) -> Option<Account> {
        None
    }
    fn storage(&self, a: Address, slot: B256) -> U256 {
        if a != validator_0x88::ADDRESS {
            return U256::ZERO;
        }
        match self.known.get(&slot) {
            Some(v) => *v,
            None => {
                self.miss.borrow_mut().insert(slot);
                U256::ZERO
            }
        }
    }
}

/// The pool of gap block `g` (state root `root`, from the verified header), every byte proven.
pub fn pool(f: &mut impl ProofFetch, root: B256, g: u64) -> Result<Vec<Address>, String> {
    let mut known: BTreeMap<B256, U256> = BTreeMap::new();
    let mut storage_root: Option<B256> = None;
    for _ in 0..ROUNDS {
        let lazy = Lazy {
            known: &known,
            miss: RefCell::default(),
        };
        let built = gap_snapshot::build(&lazy, g);
        let miss: Vec<B256> = lazy.miss.into_inner().into_iter().collect();
        if miss.is_empty() {
            return built.map(|p| p.candidates).map_err(|e| e.to_string());
        }
        for chunk in miss.chunks(BATCH) {
            let r = f.proof(chunk)?;
            verify_account(
                root,
                validator_0x88::ADDRESS,
                r.account.as_ref(),
                &r.account_proof,
            )
            .map_err(|e| e.to_string())?;
            let acc = r.account.ok_or("no 0x88 account at the gap block")?;
            if *storage_root.get_or_insert(acc.storage_root) != acc.storage_root {
                return Err("the storage root changed between answers".into());
            }
            if r.slots.len() != chunk.len() {
                return Err("the answer holds another number of slots than asked".into());
            }
            for (slot, (v, p)) in chunk.iter().zip(r.slots) {
                verify_slot(acc.storage_root, *slot, v, &p).map_err(|e| e.to_string())?;
                known.insert(*slot, v);
            }
        }
    }
    Err(format!("the pool needs more than {ROUNDS} rounds of slots"))
}

/// A JSON-RPC endpoint over HTTP(S), blocking.
pub struct Rpc {
    target: wire::Target,
    number: u64,
    last: Option<Instant>,
    pub requests: u64,
}

impl Rpc {
    /// `url`: `https://host[:port][/path]` or `http://...`. Plain HTTP is allowed: the answers are
    /// verified, so the channel needs no trust.
    pub fn new(url: &str, number: u64) -> Result<Self, String> {
        Ok(Self {
            target: wire::parse_target(url, ["https", "http"], "/")?,
            number,
            last: None,
            requests: 0,
        })
    }

    /// The address behind `hash` from the archive node's preimage table (`debug_preimage`), checked
    /// by its hash: the source is not trusted. For an account above 2^256, whose address the
    /// executor needs and snap/1 does not carry.
    pub fn preimage(&mut self, hash: B256) -> Result<Address, String> {
        let v = self.call("debug_preimage", json!([format!("{hash:#x}")]))?;
        let b = hex::decode(v.as_str().ok_or("not a string")?).map_err(|e| e.to_string())?;
        let a: [u8; 20] = b
            .as_slice()
            .try_into()
            .map_err(|_| "not 20 bytes".to_string())?;
        let a = Address::from(a);
        if alloy_primitives::keccak256(a) != hash {
            return Err("the preimage does not hash to the key".into());
        }
        Ok(a)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if let Some(t) = self.last {
            if let Some(w) = GAP.checked_sub(t.elapsed()) {
                std::thread::sleep(w);
            }
        }
        self.last = Some(Instant::now());
        self.requests += 1;
        let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
        let t = &self.target;
        let (mut s, _) =
            wire::connect(t, true, Duration::from_secs(10)).map_err(|e| e.to_string())?;
        let req = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            t.path,
            t.host,
            body.len()
        );
        s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        // a TLS peer that closes without close_notify ends the read with an error after the body
        let r = s.read_to_end(&mut raw);
        if raw.is_empty() {
            return Err(r.err().map_or("empty answer".into(), |e| e.to_string()));
        }
        let v = http_json(&raw)?;
        if let Some(e) = v.get("error") {
            return Err(format!("rpc error: {e}"));
        }
        v.get("result").cloned().ok_or_else(|| "no result".into())
    }
}

/// The JSON body of an HTTP/1.1 response (status 200, identity or chunked).
fn http_json(raw: &[u8]) -> Result<Value, String> {
    let at = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no end of headers")?;
    let head = String::from_utf8_lossy(&raw[..at]).to_ascii_lowercase();
    if !head.starts_with("http/1.1 200") && !head.starts_with("http/1.0 200") {
        return Err(format!("http {}", head.lines().next().unwrap_or("")));
    }
    let mut body = &raw[at + 4..];
    let owned;
    if head.contains("transfer-encoding: chunked") {
        let mut out = Vec::new();
        loop {
            let nl = body
                .windows(2)
                .position(|w| w == b"\r\n")
                .ok_or("bad chunk")?;
            let n = usize::from_str_radix(
                String::from_utf8_lossy(&body[..nl])
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim(),
                16,
            )
            .map_err(|_| "bad chunk size")?;
            body = &body[nl + 2..];
            if n == 0 {
                break;
            }
            out.extend_from_slice(body.get(..n).ok_or("short chunk")?);
            body = body.get(n + 2..).ok_or("short chunk")?;
        }
        owned = out;
        body = &owned;
    }
    serde_json::from_slice(body).map_err(|e| e.to_string())
}

fn quantity(v: &Value) -> Result<U256, String> {
    let s = v.as_str().ok_or("not a quantity")?;
    let h = s.strip_prefix("0x").ok_or("quantity without 0x")?;
    if h.is_empty() {
        return Err("empty quantity".into());
    }
    U256::from_str_radix(h, 16).map_err(|e| e.to_string())
}

fn hash(v: &Value) -> Result<B256, String> {
    let b = hex::decode(v.as_str().ok_or("not hex")?).map_err(|e| e.to_string())?;
    (b.len() == 32)
        .then(|| B256::from_slice(&b))
        .ok_or("not 32 bytes".into())
}

fn nodes(v: &Value) -> Result<Vec<Bytes>, String> {
    v.as_array()
        .ok_or("proof is not a list")?
        .iter()
        .map(|n| {
            hex::decode(n.as_str().ok_or("node is not hex")?)
                .map(Bytes::from)
                .map_err(|e| e.to_string())
        })
        .collect()
}

impl ProofFetch for Rpc {
    fn proof(&mut self, slots: &[B256]) -> Result<Reply, String> {
        let keys: Vec<String> = slots.iter().map(|s| format!("{s:#x}")).collect();
        let r = self.call(
            "eth_getProof",
            json!([
                format!("{:#x}", validator_0x88::ADDRESS),
                keys,
                format!("{:#x}", self.number)
            ]),
        )?;
        let balance = quantity(&r["balance"])?;
        let acc = Account {
            nonce: quantity(&r["nonce"])?.saturating_to::<u64>(),
            balance: WideBalance::from_be_bytes(&balance.to_be_bytes_trimmed_vec()),
            storage_root: hash(&r["storageHash"])?,
            code_hash: hash(&r["codeHash"])?,
        };
        let empty = acc.nonce == 0
            && balance.is_zero()
            && acc.storage_root == Account::default().storage_root
            && acc.code_hash == KECCAK256_EMPTY;
        let sp = r["storageProof"].as_array().ok_or("no storageProof")?;
        Ok(Reply {
            account: (!empty).then_some(acc),
            account_proof: nodes(&r["accountProof"])?,
            slots: sp
                .iter()
                .map(|s| Ok((quantity(&s["value"])?, nodes(&s["proof"])?)))
                .collect::<Result<_, String>>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::keccak256;
    use xone_snap::mpt::Trie;
    use xone_state::system_contracts::{dyn_array_slot, mapping_slot};

    /// A 0x88 state with `n` candidates, served as proofs; `lie` flips one slot value.
    struct Fake {
        slots: BTreeMap<B256, U256>,
        acc_trie: Trie,
        sto_trie: Trie,
        acc: Account,
        lie: bool,
        calls: usize,
    }

    fn fake(n: u64, lie: bool) -> (Fake, B256, Vec<Address>) {
        let w = |s: U256| B256::from(s.to_be_bytes::<32>());
        let mut slots = BTreeMap::new();
        slots.insert(w(U256::from(validator_0x88::CANDIDATES)), U256::from(n));
        let mut want = Vec::new();
        let mut order = Vec::new();
        for i in 0..n {
            let a = Address::repeat_byte(u8::try_from(i + 1).unwrap());
            slots.insert(
                w(dyn_array_slot(U256::from(validator_0x88::CANDIDATES), i, 1)),
                U256::from_be_slice(a.as_slice()),
            );
            let cap = U256::from(1000 + i * 10);
            slots.insert(
                w(mapping_slot(
                    B256::left_padding_from(a.as_slice()),
                    validator_0x88::VALIDATORS_STATE,
                )
                .wrapping_add(U256::from(1))),
                cap,
            );
            order.push((a, cap));
        }
        order.sort_by(|a, b| b.1.cmp(&a.1));
        want.extend(order.into_iter().map(|(a, _)| a));
        let leaves = slots
            .iter()
            .map(|(k, v)| (keccak256(k), alloy_rlp::encode(v)))
            .collect();
        let sto_trie = Trie::build(&leaves);
        let acc = Account {
            storage_root: sto_trie.root,
            ..Default::default()
        };
        let mut al = BTreeMap::new();
        al.insert(keccak256(validator_0x88::ADDRESS), acc.rlp());
        for i in 1..20u8 {
            al.insert(
                keccak256(Address::repeat_byte(0xa0 + i)),
                Account::default().rlp(),
            );
        }
        let acc_trie = Trie::build(&al);
        let root = acc_trie.root;
        (
            Fake {
                slots,
                acc_trie,
                sto_trie,
                acc,
                lie,
                calls: 0,
            },
            root,
            want,
        )
    }

    impl ProofFetch for Fake {
        fn proof(&mut self, slots: &[B256]) -> Result<Reply, String> {
            self.calls += 1;
            Ok(Reply {
                account: Some(self.acc.clone()),
                account_proof: self.acc_trie.prove(keccak256(validator_0x88::ADDRESS)),
                slots: slots
                    .iter()
                    .map(|s| {
                        let mut v = self.slots.get(s).copied().unwrap_or_default();
                        if self.lie && v == U256::from(1000) {
                            v += U256::from(1);
                        }
                        (v, self.sto_trie.prove(keccak256(s)))
                    })
                    .collect(),
            })
        }
    }

    #[test]
    fn the_pool_is_read_through_proofs_in_stake_order() {
        let (mut f, root, want) = fake(150, false);
        let got = pool(&mut f, root, 100).unwrap();
        assert_eq!(got, want);
        assert!(f.calls >= 3, "length, entries, caps are separate rounds");
    }

    #[test]
    fn a_forged_slot_value_is_refused() {
        let (mut f, root, _) = fake(10, true);
        let e = pool(&mut f, root, 100).unwrap_err();
        assert!(e.contains("storage proof"), "{e}");
    }

    #[test]
    fn a_wrong_state_root_is_refused() {
        let (mut f, _, _) = fake(10, false);
        let e = pool(&mut f, B256::repeat_byte(7), 100).unwrap_err();
        assert!(e.contains("account proof"), "{e}");
    }

    #[test]
    fn http_bodies_plain_and_chunked() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\n\r\n{\"result\":\"0x1\"}\n";
        assert_eq!(http_json(plain).unwrap()["result"], "0x1");
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\n{\"resu\r\na\r\nlt\":\"0x1\"}\r\n0\r\n\r\n";
        assert_eq!(http_json(chunked).unwrap()["result"], "0x1");
        assert!(http_json(b"HTTP/1.1 429 Too Many\r\n\r\n").is_err());
    }
}
