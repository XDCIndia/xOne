//! The XA-02 wide-balance matrix (`spikes/wide-balance/vectors/matrix.json`, printed by the geth-XDC
//! Go oracle) through the production `WideJournal` and `XdcHandler`: every balance, storage slot,
//! transaction outcome and the state root must equal geth. Adapted from the spike harness; gas is
//! not compared, as there.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::{Encodable, Header};
use alloy_trie::{HashBuilder, Nibbles, EMPTY_ROOT_HASH};
use revm::{
    context::{result::ExecutionResult, ContextSetters, TxEnv},
    context_interface::journaled_state::{account::JournaledAccountTr, JournalTr},
    handler::Handler,
    primitives::{TxKind, KECCAK_EMPTY},
    state::{Account, AccountInfo, Bytecode, EvmState},
    Database,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    convert::Infallible,
};
use xone_chainspec::apothem;
use xone_evm::{
    handler::{build_evm, XdcEvm, XdcHandler},
    journal::account_true_balance,
    spec::XdcBlock,
};
use xone_primitives::WideBalance;

#[derive(Deserialize, Clone)]
pub struct AcctIn {
    pub balance: String,
    #[serde(default)]
    pub nonce: u64,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub storage: BTreeMap<String, String>,
}
#[derive(Deserialize, Clone, Default)]
pub struct Op {
    pub op: String,
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub gas: u64,
    #[serde(default)]
    pub gas_price: String,
    #[serde(default)]
    pub data: String,
    #[serde(default)]
    pub coinbase: String,
    #[serde(default)]
    pub owner: String,
}
#[derive(Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct TxOut {
    pub consensus_err: String,
    pub vm_err: String,
    pub gas_used: u64,
}
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AcctOut {
    pub exists: bool,
    #[serde(default)]
    pub nonce: u64,
    #[serde(default)]
    pub low: String,
    #[serde(default)]
    pub r#true: String,
    #[serde(default)]
    pub wide: bool,
    #[serde(default)]
    pub code_hash: String,
    #[serde(default)]
    pub storage_root: String,
    #[serde(default)]
    pub storage: BTreeMap<String, String>,
    #[serde(default)]
    pub rlp: String,
}
#[derive(Deserialize, Clone)]
pub struct Expect {
    pub state_root: String,
    pub txs: Vec<TxOut>,
    pub accounts: BTreeMap<String, AcctOut>,
}
#[derive(Deserialize, Clone)]
pub struct Row {
    pub id: String,
    pub matrix_row: String,
    pub rule: String,
    pub accounts: BTreeMap<String, AcctIn>,
    pub ops: Vec<Op>,
    pub watch: Vec<String>,
    #[serde(default)]
    pub slots: BTreeMap<String, Vec<String>>,
    pub expect: Expect,
}

pub fn rows() -> Vec<Row> {
    serde_json::from_str(include_str!(
        "../../../spikes/wide-balance/vectors/matrix.json"
    ))
    .expect("matrix.json")
}

fn hex(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let s = if s.len() % 2 == 1 {
        format!("0{s}")
    } else {
        s.to_string()
    };
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}
fn wb(s: &str) -> WideBalance {
    WideBalance::from_be_bytes(&hex(s))
}
fn u(s: &str) -> U256 {
    U256::from_be_slice(&hex(s))
}
fn addr(s: &str) -> Address {
    Address::from_slice(&hex(s))
}
pub fn hx(w: &WideBalance) -> String {
    let b = w.to_be_bytes();
    if b.is_empty() {
        return "0x0".into();
    }
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("0x{}", h.trim_start_matches('0'))
}

#[derive(Default)]
pub struct MemAcct {
    pub info: AccountInfo,
    pub storage: BTreeMap<U256, U256>,
}
#[derive(Default)]
pub struct Mem {
    pub accounts: BTreeMap<Address, MemAcct>,
    codes: HashMap<B256, Bytecode>,
}
impl Database for Mem {
    type Error = Infallible;
    fn basic(&mut self, a: Address) -> Result<Option<AccountInfo>, Infallible> {
        Ok(self.accounts.get(&a).map(|m| m.info.clone()))
    }
    fn code_by_hash(&mut self, h: B256) -> Result<Bytecode, Infallible> {
        Ok(self.codes.get(&h).cloned().unwrap_or_default())
    }
    fn storage(&mut self, a: Address, k: U256) -> Result<U256, Infallible> {
        Ok(self
            .accounts
            .get(&a)
            .and_then(|m| m.storage.get(&k).copied())
            .unwrap_or_default())
    }
    fn block_hash(&mut self, _: u64) -> Result<B256, Infallible> {
        Ok(B256::ZERO)
    }
}

/// Account leaf exactly as geth `StateAccount.EncodeRLP` writes it.
pub fn encode_account(nonce: u64, bal: &WideBalance, root: B256, code_hash: B256) -> Vec<u8> {
    let mut p = Vec::new();
    nonce.encode(&mut p);
    bal.encode(&mut p);
    root.encode(&mut p);
    code_hash.encode(&mut p);
    wrap_list(p)
}
/// Snap `AccountRange` item: geth `types.SlimAccountRLP`.
pub fn encode_slim(nonce: u64, bal: &WideBalance, root: B256, code_hash: B256) -> Vec<u8> {
    let mut p = Vec::new();
    nonce.encode(&mut p);
    bal.encode(&mut p);
    if root == EMPTY_ROOT_HASH {
        [].as_slice().encode(&mut p)
    } else {
        root.encode(&mut p)
    }
    if code_hash == KECCAK_EMPTY {
        [].as_slice().encode(&mut p)
    } else {
        code_hash.encode(&mut p)
    }
    wrap_list(p)
}
fn wrap_list(p: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    Header {
        list: true,
        payload_length: p.len(),
    }
    .encode(&mut out);
    out.extend_from_slice(&p);
    out
}

pub fn storage_root(storage: &BTreeMap<U256, U256>) -> B256 {
    let sorted: BTreeMap<B256, Vec<u8>> = storage
        .iter()
        .filter(|(_, v)| !v.is_zero())
        .map(|(k, v)| {
            let mut e = Vec::new();
            v.encode(&mut e);
            (keccak256(k.to_be_bytes::<32>()), e)
        })
        .collect();
    if sorted.is_empty() {
        return EMPTY_ROOT_HASH;
    }
    let mut hb = HashBuilder::default();
    for (k, v) in sorted {
        hb.add_leaf(Nibbles::unpack(k), &v);
    }
    hb.root()
}

pub struct Final {
    pub root: B256,
    pub txs: Vec<TxOut>,
    pub accounts: BTreeMap<String, AcctOut>,
}

fn block_of(rule: &str) -> u64 {
    if rule == "cancun" {
        72_000_000
    } else {
        40_000_000
    }
}

fn true_of(m: &MemAcct, side: Option<&WideBalance>) -> WideBalance {
    account_true_balance(&Account::from(m.info.clone()), side)
}

pub fn run(row: &Row) -> Final {
    let mut mem = Mem::default();
    let mut seeds = Vec::new();
    for (a, i) in &row.accounts {
        let code = Bytecode::new_raw(Bytes::from(hex(&i.code)));
        let tru = wb(&i.balance);
        let info = AccountInfo {
            balance: tru.low(),
            nonce: i.nonce,
            code_hash: if i.code.is_empty() {
                KECCAK_EMPTY
            } else {
                code.hash_slow()
            },
            code: Some(code.clone()),
            ..Default::default()
        };
        if tru.is_wide() {
            seeds.push((addr(a), tru));
        }
        mem.codes.insert(info.code_hash, code);
        let storage = i.storage.iter().map(|(k, v)| (u(k), u(v))).collect();
        mem.accounts.insert(addr(a), MemAcct { info, storage });
    }

    let number = block_of(&row.rule);
    let block = XdcBlock {
        number,
        gas_limit: 30_000_000,
        difficulty: U256::from(1),
        base_fee: (row.rule == "cancun").then_some(0),
        ..Default::default()
    };
    let mut evm: XdcEvm<Mem> = build_evm(mem, &apothem(), &block, None).expect("rules");
    for (a, w) in seeds {
        evm.ctx.journaled_state.side.insert(a, w);
    }

    let mut snaps: Vec<(
        String,
        revm::context_interface::journaled_state::JournalCheckpoint,
    )> = Vec::new();
    let mut txs = Vec::new();
    for op in &row.ops {
        match op.op.as_str() {
            "add" | "sub" | "set_balance" => {
                let j = &mut evm.ctx.journaled_state;
                let mut acc = j.load_account_mut(addr(&op.addr)).expect("infallible").data;
                let v = u(&op.amount);
                match op.op.as_str() {
                    "add" => assert!(acc.incr_balance(v)),
                    "sub" => assert!(acc.decr_balance(v)),
                    _ => acc.set_balance(v),
                }
            }
            "snapshot" => {
                let cp = evm.ctx.journaled_state.checkpoint();
                snaps.push((op.name.clone(), cp));
            }
            "revert" => {
                // geth RevertToSnapshot drops every later snapshot: commit them, then revert.
                while snaps.last().is_some_and(|(n, _)| *n != op.name) {
                    snaps.pop();
                    evm.ctx.journaled_state.checkpoint_commit();
                }
                let (_, cp) = snaps.pop().expect("known snapshot");
                evm.ctx.journaled_state.checkpoint_revert(cp);
            }
            "tx" => {
                flush(&mut evm);
                txs.push(run_tx(&mut evm, op));
                flush(&mut evm);
            }
            other => panic!("unknown op {other}"),
        }
    }
    flush(&mut evm);

    let j = &evm.ctx.journaled_state;
    let mem = &j.inner.database;
    let mut leaves: BTreeMap<B256, Vec<u8>> = BTreeMap::new();
    let mut accounts = BTreeMap::new();
    let mut per_addr: HashMap<Address, (u64, WideBalance, B256, B256, Vec<u8>)> = HashMap::new();
    for (a, m) in &mem.accounts {
        let tru = true_of(m, j.side.get(a));
        let root = storage_root(&m.storage);
        let rlp = encode_account(m.info.nonce, &tru, root, m.info.code_hash);
        leaves.insert(keccak256(a), rlp.clone());
        per_addr.insert(*a, (m.info.nonce, tru, root, m.info.code_hash, rlp));
    }
    let root = if leaves.is_empty() {
        EMPTY_ROOT_HASH
    } else {
        let mut hb = HashBuilder::default();
        for (k, v) in &leaves {
            hb.add_leaf(Nibbles::unpack(*k), v);
        }
        hb.root()
    };
    for w in &row.watch {
        let a = addr(w);
        let out = match per_addr.get(&a) {
            None => AcctOut {
                exists: false,
                nonce: 0,
                low: String::new(),
                r#true: String::new(),
                wide: false,
                code_hash: String::new(),
                storage_root: String::new(),
                storage: BTreeMap::new(),
                rlp: String::new(),
            },
            Some((nonce, tru, sroot, ch, rlp)) => AcctOut {
                exists: true,
                nonce: *nonce,
                low: hx(&WideBalance::from(tru.low())),
                r#true: hx(tru),
                wide: tru.is_wide(),
                code_hash: format!("{ch:?}"),
                storage_root: format!("{sroot:?}"),
                storage: row
                    .slots
                    .get(w)
                    .map(|ks| {
                        ks.iter()
                            .map(|k| {
                                let v = mem.accounts[&a]
                                    .storage
                                    .get(&u(k))
                                    .copied()
                                    .unwrap_or_default();
                                (k.clone(), format!("0x{}", hex_of(&v.to_be_bytes::<32>())))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                rlp: format!("0x{}", hex_of(rlp)),
            },
        };
        accounts.insert(w.clone(), out);
    }
    Final {
        root,
        txs,
        accounts,
    }
}

fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn run_tx(evm: &mut XdcEvm<Mem>, op: &Op) -> TxOut {
    let from = addr(&op.from);
    let nonce = evm
        .ctx
        .journaled_state
        .load_account(from)
        .expect("infallible")
        .data
        .info
        .nonce;
    evm.ctx.block.beneficiary = addr(&op.coinbase);
    // geth pays the coinbase owner after TIPTRC21Fee; the oracle's owner defaults to the coinbase.
    evm.ctx.chain.fee_owner = Some(if op.owner.is_empty() {
        addr(&op.coinbase)
    } else {
        addr(&op.owner)
    });
    let tx = TxEnv {
        tx_type: 0,
        caller: from,
        gas_limit: op.gas,
        gas_price: u128::try_from(u(&op.gas_price)).expect("price"),
        kind: if op.to.is_empty() {
            TxKind::Create
        } else {
            TxKind::Call(addr(&op.to))
        },
        value: u(&op.value),
        data: Bytes::from(hex(&op.data)),
        nonce,
        chain_id: None,
        ..Default::default()
    };
    evm.ctx.set_tx(tx);
    let res = XdcHandler::<Mem>::default().run(evm);
    let mut out = TxOut {
        consensus_err: String::new(),
        vm_err: String::new(),
        gas_used: 0,
    };
    match res {
        Err(e) => out.consensus_err = format!("{e:?}"),
        Ok(ExecutionResult::Success { gas, .. }) => out.gas_used = gas.tx_gas_used(),
        Ok(ExecutionResult::Revert { gas, .. }) => {
            out.vm_err = "revert".into();
            out.gas_used = gas.tx_gas_used();
        }
        Ok(ExecutionResult::Halt { gas, reason, .. }) => {
            out.vm_err = format!("{reason:?}");
            out.gas_used = gas.tx_gas_used();
        }
    }
    out
}

/// Apply the journal's pending state to the DB (geth `Finalise`: delete selfdestructed and,
/// EIP-161, empty touched accounts).
fn flush(evm: &mut XdcEvm<Mem>) {
    let state: EvmState = evm.ctx.journaled_state.finalize();
    let j = &mut evm.ctx.journaled_state;
    for (a, acct) in state {
        if !acct.is_touched() && !acct.is_created() && !acct.is_selfdestructed() {
            continue;
        }
        if acct.is_selfdestructed() || acct.is_empty() {
            j.inner.database.accounts.remove(&a);
            j.side.forget(&a);
            continue;
        }
        let m = j.inner.database.accounts.entry(a).or_default();
        m.info = acct.info.clone();
        if let Some(c) = &acct.info.code {
            j.inner
                .database
                .codes
                .insert(acct.info.code_hash, c.clone());
        }
        for (k, s) in acct.storage {
            if s.is_changed() {
                if s.present_value.is_zero() {
                    m.storage.remove(&k);
                } else {
                    m.storage.insert(k, s.present_value);
                }
            }
        }
    }
}

/// Differences between the oracle and a run; empty means the row matches.
pub fn diff(row: &Row, got: &Final) -> Vec<String> {
    let mut d = Vec::new();
    let e = &row.expect;
    if format!("{:?}", got.root) != e.state_root {
        d.push(format!("state_root {:?} != {}", got.root, e.state_root));
    }
    if got.txs.len() != e.txs.len() {
        d.push("tx count".into());
    }
    for (i, (g, x)) in got.txs.iter().zip(&e.txs).enumerate() {
        if g.consensus_err.is_empty() != x.consensus_err.is_empty() {
            d.push(format!(
                "tx{i} consensus_err got {:?} want {:?}",
                g.consensus_err, x.consensus_err
            ));
        }
        if g.vm_err.is_empty() != x.vm_err.is_empty() {
            d.push(format!(
                "tx{i} vm_err got {:?} want {:?}",
                g.vm_err, x.vm_err
            ));
        }
    }
    for (a, want) in &e.accounts {
        let g = &got.accounts[a];
        let tag = &a[a.len() - 4..];
        if g.exists != want.exists {
            d.push(format!(
                "{tag} exists got {} want {}",
                g.exists, want.exists
            ));
            continue;
        }
        if !g.exists {
            continue;
        }
        for (n, gv, wv) in [
            ("low", &g.low, &want.low),
            ("true", &g.r#true, &want.r#true),
            ("rlp", &g.rlp, &want.rlp),
        ] {
            if gv != wv {
                d.push(format!("{tag} {n} got {gv} want {wv}"));
            }
        }
        if g.nonce != want.nonce
            || g.wide != want.wide
            || g.code_hash != want.code_hash
            || g.storage_root != want.storage_root
            || g.storage != want.storage
        {
            d.push(format!(
                "{tag} nonce/wide/code/storage differ: got {g:?} want {want:?}"
            ));
        }
    }
    d
}

#[test]
fn matrix_equals_geth() {
    let all = rows();
    assert_eq!(all.len(), 40);
    let bad: Vec<(String, String)> = all
        .iter()
        .filter_map(|r| {
            let d = diff(r, &run(r));
            (!d.is_empty()).then(|| (r.id.clone(), d.join("; ")))
        })
        .collect();
    assert!(bad.is_empty(), "{bad:#?}");
}
