#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! The sibling shapes of issue #33 on real blocks: net2040 619,712/619,713 and Apothem
//! 85,209,326/85,209,327 and 85,214,323/85,214,324. The archive serves the canonical blocks only
//! (`tools/fetch_shapes.py`); the stale sibling S of the canonical block X is X with another
//! timestamp, and T is an empty child of S. Each shape runs every `shape/*` scenario of the geth
//! oracle (`vectors/fork_choice.json`): s3 is the real parent, m4 = X, m5 = X's real child Y,
//! a4 = S, a5 = T. The real bodies and receipts go through `BodyVerifier` on every import.

mod common;
use alloy_primitives::{hex, Address, Bytes, B256, U256};
use alloy_rlp::Decodable;
use alloy_trie::EMPTY_ROOT_HASH;
use common::{accept, scenarios, Accept, Mode, Mutant};
use serde::Deserialize;
use std::{collections::BTreeMap, str::FromStr};
use xone_chainspec::{apothem, devnet, ChainSpec, ForkBlock};
use xone_consensus::verify::{Body, BodyError};
use xone_primitives::{BlockInfo, ExtraFieldsV2, Header, QuorumCert, WideBalance};
use xone_state::{Account, FlatRead, FlatWrite, MemFlat, StateDiff, StateRead};
use xone_sync::{
    chain::{Chain, ChainRead, InsertError},
    executor::Execution,
    store::Block,
    testkit::{MemStore, VectorExecutor},
};

#[derive(Deserialize, Clone)]
struct Rec {
    nonce: u64,
    balance: String,
    code_hash: String,
    storage: BTreeMap<String, String>,
}

#[derive(Deserialize, Clone)]
struct Pair {
    pre: Option<Rec>,
    post: Option<Rec>,
}

#[derive(Deserialize)]
struct Blk {
    number: u64,
    hash: String,
    header: String,
    txs: Vec<String>,
    withdrawals: bool,
    receipts: Vec<String>,
}

#[derive(Deserialize)]
struct Shape {
    name: String,
    network: String,
    anchor: String,
    x: Blk,
    y: Blk,
    x_diff: Option<BTreeMap<String, Pair>>,
    y_diff: Option<BTreeMap<String, Pair>>,
}

fn bytes(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap();
    hex::decode(if s.len() % 2 == 1 {
        format!("0{s}")
    } else {
        s.into()
    })
    .unwrap()
}

fn account(r: &Rec) -> Account {
    Account {
        nonce: r.nonce,
        balance: WideBalance::from_be_bytes(&bytes(&r.balance)),
        code_hash: B256::from_str(&r.code_hash).unwrap(),
        ..Default::default()
    }
}

fn header(hex: &str) -> Header {
    Header::decode(&mut bytes(hex).as_slice()).unwrap()
}

fn block(b: &Blk) -> Block {
    let h = header(&b.header);
    assert_eq!(
        h.hash(),
        B256::from_str(&b.hash).unwrap(),
        "RLP parity of #{}",
        b.number
    );
    Block::new(
        h,
        Body {
            transactions: b.txs.iter().map(|t| Bytes::from(bytes(t))).collect(),
            uncles: vec![],
            withdrawals: b.withdrawals.then(Vec::new),
        },
    )
}

type Pre = (
    BTreeMap<Address, Option<Account>>,
    BTreeMap<(Address, B256), U256>,
);

/// (pre accounts and slots, diff) of one recorded block diff.
fn parts(d: &BTreeMap<String, Pair>) -> (Pre, StateDiff) {
    let (mut pre, mut diff) = (Pre::default(), StateDiff::default());
    for (a, p) in d {
        let a = Address::from_str(&a.to_lowercase()).unwrap();
        pre.0.insert(a, p.pre.as_ref().map(account));
        diff.accounts.insert(a, p.post.as_ref().map(account));
        for (s, v) in p.pre.iter().flat_map(|r| &r.storage) {
            pre.1.insert(
                (a, B256::from_str(s).unwrap()),
                U256::from_be_slice(&bytes(v)),
            );
        }
        for (s, v) in p.post.iter().flat_map(|r| &r.storage) {
            diff.storage.insert(
                (a, B256::from_str(s).unwrap()),
                U256::from_be_slice(&bytes(v)),
            );
        }
    }
    (pre, diff)
}

struct Built {
    spec: ChainSpec,
    anchor: Header,
    flat: MemFlat,
    exec: VectorExecutor,
    labels: BTreeMap<&'static str, Block>,
    x_post: MemFlat,
    touched: Vec<(Address, Option<Account>)>,
}

fn build(sh: &Shape, v2: bool, edit: &dyn Fn(&mut Header)) -> Built {
    let mut spec = if sh.network == "apothem" {
        apothem()
    } else {
        devnet()
    };
    if sh.network == "net2040" {
        // The net2040 headers carry the block access list hash: Amsterdam is active.
        spec.amsterdam = ForkBlock::At(0);
    }
    match (v2, sh.network.as_str()) {
        (false, _) => spec.xdpos.v2 = None,
        (true, "apothem") => {}
        (true, _) => spec.xdpos.v2.as_mut().unwrap().switch_block = 0,
    }
    let anchor = header(&sh.anchor);
    let (x, y) = (block(&sh.x), block(&sh.y));
    // The honest execution of X is recorded under the hash of the edited header too.
    let (root, gas, bal) = (
        x.header.state_root,
        x.header.gas_used,
        x.header.block_access_list_hash,
    );
    let x = {
        let mut h = x.header.clone();
        edit(&mut h);
        Block::new(h, x.body.clone())
    };
    let ((xpre, xslots), xdiff) = parts(sh.x_diff.as_ref().unwrap());
    let empty = BTreeMap::new();
    let ((ypre, yslots), ydiff) = parts(sh.y_diff.as_ref().unwrap_or(&empty));
    // The anchor state: the accounts X reads, plus those Y reads that X does not write.
    let mut flat = MemFlat::default();
    for (a, acc) in &xpre {
        flat.set_account(*a, acc.clone());
    }
    for ((a, s), v) in &xslots {
        flat.set_slot(*a, *s, *v);
    }
    let mut x_post = flat.clone();
    xdiff.apply(&mut x_post).unwrap();
    for (a, acc) in &ypre {
        if !xdiff.accounts.contains_key(a) {
            flat.set_account(*a, acc.clone());
            x_post.set_account(*a, acc.clone());
        } else {
            assert_eq!(*acc, x_post.account(*a), "Y reads what X wrote");
        }
    }
    for ((a, s), v) in &yslots {
        if !xdiff.storage.contains_key(&(*a, *s)) {
            flat.set_slot(*a, *s, *v);
            x_post.set_slot(*a, *s, *v);
        } else {
            assert_eq!(*v, x_post.storage(*a, *s), "Y reads the slot X wrote");
        }
    }
    let recv = |b: &Blk| {
        b.receipts
            .iter()
            .map(|r| Bytes::from(bytes(r)))
            .collect::<Vec<_>>()
    };
    let ex = |blk: &Block, diff: StateDiff, r: Vec<Bytes>| Execution {
        diff,
        receipts: r,
        state_root: if blk.hash() == x.hash() || blk.parent() == x.parent() {
            root
        } else {
            blk.header.state_root
        },
        gas_used: if blk.hash() == x.hash() || blk.parent() == x.parent() {
            gas
        } else {
            blk.header.gas_used
        },
        access_list_hash: if blk.hash() == x.hash() || blk.parent() == x.parent() {
            bal
        } else {
            blk.header.block_access_list_hash
        },
        aux: Default::default(),
        roots: None,
    };
    let mut exec = VectorExecutor::default();
    exec.record(
        x.hash(),
        xpre.clone(),
        xslots.clone(),
        ex(&x, xdiff.clone(), recv(&sh.x)),
    );
    exec.record(y.hash(), ypre, yslots, ex(&y, ydiff.clone(), recv(&sh.y)));
    // S: the same transactions in another block of the same height.
    let mut sh_ = x.header.clone();
    sh_.timestamp += 1;
    let s = Block::new(sh_, x.body.clone());
    exec.record(s.hash(), xpre, xslots, ex(&s, xdiff.clone(), recv(&sh.x)));
    // T: an empty child of S.
    let round = s.header.extra_fields_v2().map_or(0, |f| f.round);
    let mut th = Header {
        parent_hash: s.hash(),
        number: s.number() + 1,
        timestamp: s.header.timestamp + 2,
        tx_root: EMPTY_ROOT_HASH,
        receipt_root: EMPTY_ROOT_HASH,
        bloom: Default::default(),
        gas_used: 0,
        extra_data: Default::default(),
        validators: Default::default(),
        validator: Default::default(),
        penalties: Default::default(),
        withdrawals_hash: s.header.withdrawals_hash.map(|_| EMPTY_ROOT_HASH),
        ..s.header.clone()
    };
    if v2 {
        th.extra_data = ExtraFieldsV2 {
            round: round + 1,
            quorum_cert: QuorumCert {
                proposed_block_info: BlockInfo {
                    hash: s.hash(),
                    round,
                    number: s.number(),
                },
                signatures: vec![],
                gap_number: 0,
            },
        }
        .encode_extra()
        .into();
    }
    let t = Block::new(
        th,
        Body {
            withdrawals: x.body.withdrawals.clone(),
            ..Default::default()
        },
    );
    exec.record(
        t.hash(),
        Default::default(),
        Default::default(),
        ex(&t, StateDiff::default(), vec![]),
    );
    let mut touched: Vec<(Address, Option<Account>)> = vec![];
    for a in xdiff.accounts.keys().chain(ydiff.accounts.keys()) {
        let mut m = x_post.clone();
        ydiff.apply(&mut m).unwrap();
        touched.push((*a, m.account(*a)));
    }
    Built {
        spec,
        anchor,
        flat,
        exec,
        labels: BTreeMap::from([("m4", x), ("m5", y), ("a4", s), ("a5", t)]),
        x_post,
        touched,
    }
}

fn shapes() -> Vec<Shape> {
    serde_json::from_str(include_str!("vectors/shapes.json")).unwrap()
}

#[test]
fn real_blocks_choose_like_geth() {
    let (mut runs, mut events) = (0, 0);
    for sh in shapes() {
        for sc in scenarios().iter().filter(|s| s.name.starts_with("shape/")) {
            let v2 = sc.mode == "v2";
            let b = build(&sh, v2, &|_| {});
            let anchor_hash = b.anchor.hash();
            let anchor_number = b.anchor.number;
            let store = MemStore::with_anchor(b.anchor.clone(), b.flat.clone(), 64, 100_000);
            let chain = Chain::new(b.spec.clone(), store, b.exec, accept as Accept);
            let hash_of = |l: &str| {
                if l == "s3" {
                    anchor_hash
                } else {
                    b.labels[l].hash()
                }
            };
            let number_of = |l: &str| anchor_number + l[1..].parse::<u64>().unwrap() - 3;
            for ev in &sc.events {
                let what = format!("{} {} {} {}", sh.name, sc.name, ev.block, ev.err);
                chain
                    .insert(b.labels[ev.block.as_str()].clone(), 0)
                    .unwrap_or_else(|e| panic!("{what}: {e}"));
                assert_eq!(
                    chain.head(),
                    (number_of(&ev.head), hash_of(&ev.head)),
                    "{what}: head"
                );
                for (i, l) in ev.canonical.iter().enumerate().skip(2) {
                    let n = number_of(l);
                    assert_eq!(
                        chain.reader().canonical_hash(n),
                        Some(hash_of(l)),
                        "{what}: #{n} ({i})"
                    );
                }
                events += 1;
            }
            // The head state carries the diff of X (and of Y when Y is canonical): the real writes.
            let head = chain.head().1;
            let y_in = chain.reader().canonical_hash(b.labels["m5"].number())
                == Some(b.labels["m5"].hash());
            chain
                .reader()
                .with_state(head, &mut |s: &dyn StateRead| {
                    for (a, want) in &b.touched {
                        // Canonical Y: after both diffs. Otherwise after X (S carries X's diff).
                        let expect = if y_in {
                            want.clone()
                        } else {
                            b.x_post.account(*a)
                        };
                        assert_eq!(s.account(*a), expect, "{} {}: {a}", sh.name, sc.name);
                    }
                })
                .unwrap();
            runs += 1;
        }
    }
    println!("real shapes: {runs} runs, {events} events, 0 mismatches with geth");
    assert_eq!(runs, 24);
}

#[test]
fn a_sibling_runs_on_its_own_parent_not_on_the_head() {
    // [S, X, Y]: X is executed while S is the head. On the head's state its recorded pre-state does
    // not hold, so the mutant store fails the import; the real store accepts it.
    let sh = &shapes()[2]; // Apothem 85,214,323: two transactions, real receipts
    let b = build(sh, true, &|_| {});
    let mk = |s: &Built| MemStore::with_anchor(s.anchor.clone(), s.flat.clone(), 64, 100_000);
    let chain = Chain::new(
        b.spec.clone(),
        Mutant::new(mk(&b), Mode::HeadState),
        b.exec,
        accept as Accept,
    );
    chain.insert(b.labels["a4"].clone(), 0).unwrap();
    let r = chain.insert(b.labels["m4"].clone(), 0);
    assert!(matches!(r, Err(InsertError::Exec(_))), "{r:?}");
}

#[test]
fn receipts_and_body_are_checked_on_real_blocks() {
    let sh = &shapes()[2]; // two transactions with real receipts
    let run = |sh: &Shape, edit: &dyn Fn(&mut Header)| {
        let b = build(sh, true, edit);
        let store = MemStore::with_anchor(b.anchor.clone(), b.flat.clone(), 64, 100_000);
        let chain = Chain::new(b.spec.clone(), store, b.exec, accept as Accept);
        let r = chain.insert(b.labels["m4"].clone(), 0);
        if r.is_err() {
            assert_eq!(chain.head().0, b.anchor.number, "head moved");
            assert!(
                chain.reader().header(b.labels["m4"].hash()).is_none(),
                "a rejected block was stored"
            );
        }
        r
    };
    assert_eq!(run(sh, &|_| {}), Ok(xone_sync::chain::Outcome::Extended));
    let r = run(sh, &|h| h.receipt_root = B256::repeat_byte(1));
    assert!(
        matches!(
            r,
            Err(InsertError::Body(BodyError::ReceiptRootMismatch { .. }))
        ),
        "{r:?}"
    );
    let r = run(sh, &|h| h.tx_root = B256::repeat_byte(1));
    assert!(
        matches!(r, Err(InsertError::Body(BodyError::TxRootMismatch { .. }))),
        "{r:?}"
    );
    let r = run(sh, &|h| h.bloom.0[0] ^= 1);
    assert!(
        matches!(r, Err(InsertError::Body(BodyError::BloomMismatch { .. }))),
        "{r:?}"
    );
    let r = run(sh, &|h| h.gas_used += 1);
    assert!(matches!(r, Err(InsertError::GasUsed { .. })), "{r:?}");
    let r = run(sh, &|h| h.state_root = B256::repeat_byte(1));
    assert!(matches!(r, Err(InsertError::StateRoot { .. })), "{r:?}");
    // A flipped byte of a stored receipt or transaction is caught by the roots, not decoded away.
    let flip = |v: &mut String| {
        let n = v.len();
        v.replace_range(n - 2..n, if v.ends_with("00") { "01" } else { "00" });
    };
    let mut bad = shapes().remove(2);
    flip(&mut bad.x.receipts[0]);
    assert!(matches!(run(&bad, &|_| {}), Err(InsertError::Body(_))));
    let mut bad = shapes().remove(2);
    flip(&mut bad.x.txs[0]);
    assert!(matches!(run(&bad, &|_| {}), Err(InsertError::Body(_))));
    // net2040 is past Amsterdam: the block access list hash of the header is compared with the
    // executor's (geth `ValidateState`, block_validator.go:247).
    let net = &shapes()[0];
    assert_eq!(run(net, &|_| {}), Ok(xone_sync::chain::Outcome::Extended));
    let r = run(net, &|h| {
        h.block_access_list_hash = Some(B256::repeat_byte(1))
    });
    assert!(
        matches!(r, Err(InsertError::AccessListHash { .. })),
        "{r:?}"
    );
}
