#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! Server and client against each other: an in-memory state of 10,000 accounts (wide balances,
//! storage, code) goes through the real codec and the real server, and the downloaded state must
//! hash to the exact root.

use alloy_primitives::{keccak256, Bytes, B256, B512, U256};
use std::collections::{BTreeMap, BTreeSet};
use xone_snap::{
    client::*,
    codec::*,
    mpt::Trie,
    proof::ProofError,
    server::*,
    testkit::{addr, flat_state, Loopback},
};
use xone_state::{trie::flat_root, FlatRead, FlatWrite, MemFlat};

fn server(flat: &MemFlat, codes: &BTreeMap<B256, Bytes>) -> Server {
    Server::new(Snapshot::from_flat(flat, codes.clone()), Limits::default())
}

fn small() -> Config {
    Config {
        request_bytes: 32 * 1024,
        ..Config::default()
    }
}

fn counts(log: &[Message]) -> [usize; 4] {
    let mut c = [0; 4];
    for m in log {
        match m {
            Message::GetAccountRange(_) => c[0] += 1,
            Message::GetStorageRanges(_) => c[1] += 1,
            Message::GetByteCodes(_) => c[2] += 1,
            Message::GetTrieNodes(_) => c[3] += 1,
            _ => {}
        }
    }
    c
}

#[test]
fn ten_thousand_accounts_sync_to_the_exact_root() {
    let (flat, codes) = flat_state(10_000);
    let want = flat_root(&flat); // xone-state's own root, not the snapshot's
    let mut peer = Loopback::new(server(&flat, &codes));
    assert_eq!(peer.server.snapshot().root(), want);
    let mut store = StateStore::new(want);
    sync(&mut store, &mut peer, &small()).unwrap();

    assert_eq!(store.progress.verified, Some(want));
    assert_eq!(store.accounts.len(), 10_000);
    assert_eq!(store.accounts.len(), flat.accounts().count());
    let wide = store
        .accounts
        .values()
        .filter(|a| a.balance.is_wide())
        .count();
    let slots: usize = store.storage.values().map(|s| s.len()).sum();
    let flat_slots: usize = flat.accounts().map(|(a, _)| flat.slots(a).count()).sum();
    assert_eq!(slots, flat_slots);
    assert_eq!(store.codes, codes);
    assert!(wide >= 100, "wide balances must be in the state ({wide})");
    // account 0 has 3,000 slots: more than one request's worth, so it came in chunks with proofs
    let chunked = peer
        .log
        .iter()
        .filter(|m| matches!(m, Message::GetStorageRanges(r) if !r.origin.is_empty()))
        .count();
    assert!(chunked >= 1, "a large storage trie is fetched in ranges");
    let [a, s, c, t] = counts(&peer.log);
    assert_eq!(t, 0, "a state that is not stale needs no heal request");
    println!("10,000 accounts, {wide} wide, {slots} slots, {} codes: {a} account, {s} storage ({chunked} chunked), {c} code, {t} node requests; root {want}", codes.len());
    // the state it serves back is the same state
    assert_eq!(store.snapshot().root(), want);
}

/// A kill at every request count: the run stops, the store is what a disk would hold, and the
/// next run finishes without asking again for what the store has.
#[test]
fn interrupted_sync_resumes_without_redownloading() {
    let (flat, codes) = flat_state(3_000);
    let want = flat_root(&flat);
    let mut resumed = 0;
    let total = {
        let mut p = Loopback::new(server(&flat, &codes));
        sync(&mut StateStore::new(want), &mut p, &small()).unwrap();
        p.log.len()
    };
    for kill in [0, 1, 2, 3, 5, 8, 13, total / 2, total - 1] {
        let mut store = StateStore::new(want);
        let mut first = Loopback::new(server(&flat, &codes));
        first.budget = Some(kill);
        let r = sync(&mut store, &mut first, &small());
        assert!(
            matches!(r, Err(SyncError::Peer(PeerError::Disconnected))),
            "kill {kill}: {r:?}"
        );
        // the "disk" at the moment of the kill
        let disk = store.clone();
        let mut second = Loopback::new(server(&flat, &codes));
        sync(&mut store, &mut second, &small()).unwrap();
        assert_eq!(store.progress.verified, Some(want), "kill {kill}");
        for m in &second.log {
            match m {
                Message::GetAccountRange(r) => {
                    let next = disk.progress.next_account.expect("accounts were not done");
                    assert!(
                        r.origin >= next,
                        "kill {kill}: account range from {} is below the cursor {next}",
                        r.origin
                    );
                }
                Message::GetStorageRanges(r) => {
                    for a in &r.accounts {
                        assert_ne!(
                            disk.progress.storage_next.get(a),
                            Some(&None),
                            "kill {kill}: storage of a finished account"
                        );
                    }
                    // an account that was partly done goes on from its cursor, never from zero
                    if let Some(Some(o)) = r
                        .accounts
                        .first()
                        .and_then(|a| disk.progress.storage_next.get(a))
                    {
                        assert!(
                            r.origin.len() == 32 && B256::from_slice(&r.origin) >= *o,
                            "kill {kill}: restarted a partial storage trie"
                        );
                    }
                }
                Message::GetByteCodes(r) => {
                    assert!(
                        r.hashes.iter().all(|h| !disk.codes.contains_key(h)),
                        "kill {kill}: code that was stored"
                    );
                }
                _ => {}
            }
        }
        if !disk.accounts.is_empty() {
            resumed += 1;
        }
        // killed runs plus the resumed one asked for no more than a fresh run, plus the request
        // that was in flight at the kill
        let fresh = {
            let mut p = Loopback::new(server(&flat, &codes));
            sync(&mut StateStore::new(want), &mut p, &small()).unwrap();
            p.log.len()
        };
        assert!(
            first.log.len() + second.log.len() <= fresh + 1,
            "kill {kill}: {} + {} > {fresh} + 1",
            first.log.len(),
            second.log.len()
        );
    }
    assert!(resumed >= 6, "most kills land after some data was stored");
}

fn changed(n: usize) -> (MemFlat, BTreeMap<B256, Bytes>) {
    let (mut flat, mut codes) = flat_state(n);
    for i in (0..2_000).step_by(13) {
        if let Some(mut a) = flat.account(addr(i)) {
            a.nonce += 1;
            flat.set_account(addr(i), Some(a));
        }
    }
    for i in (0..2_000).step_by(1000).skip(1) {
        flat.set_account(addr(i), None); // deleted
    }
    // the big contract: one value changed, one slot cleared, one added
    let slot = |s: usize| keccak256(format!("slot0-{s}"));
    flat.set_slot(addr(0), slot(5), U256::from(123_456u64));
    flat.set_slot(addr(0), slot(7), U256::ZERO);
    flat.set_slot(addr(0), slot(4000), U256::from(1u8));
    codes.insert(keccak256(b"unused"), Bytes::from_static(b"unused"));
    (flat, codes)
}

/// The pivot moves while the state downloads: what came from the old root stays, the rest comes
/// from the new one, and the heal pass makes the result exactly the new state.
#[test]
fn moved_pivot_is_healed_to_the_new_root() {
    let (flat_a, codes_a) = flat_state(2_000);
    let (flat_b, codes_b) = changed(2_300);
    let (root_a, root_b) = (flat_root(&flat_a), flat_root(&flat_b));
    assert_ne!(root_a, root_b);
    let total_nodes = Snapshot::from_flat(&flat_b, codes_b.clone()).account_node_count();
    let mut healed_any = false;
    let total_a = {
        let mut p = Loopback::new(server(&flat_a, &codes_a));
        sync(&mut StateStore::new(root_a), &mut p, &small()).unwrap();
        p.log.len()
    };
    for kill in [1usize, 3, 7, total_a - 1, usize::MAX] {
        let mut store = StateStore::new(root_a);
        let mut a = Loopback::new(server(&flat_a, &codes_a));
        a.budget = Some(kill);
        let r = sync(&mut store, &mut a, &small());
        assert_eq!(r.is_ok(), kill == usize::MAX, "kill {kill}: {r:?}");
        if kill == usize::MAX {
            assert_eq!(store.progress.verified, Some(root_a));
        }
        store.set_root(root_b);
        assert_eq!(store.progress.verified, None);
        let mut b = Loopback::new(server(&flat_b, &codes_b));
        sync(&mut store, &mut b, &small()).unwrap_or_else(|e| panic!("kill {kill}: {e}"));
        assert_eq!(store.progress.verified, Some(root_b), "kill {kill}");
        let want = Snapshot::from_flat(&flat_b, codes_b.clone());
        assert_eq!(store.snapshot().root(), want.root());
        assert_eq!(store.accounts.len(), want.account_count());
        let [_, _, _, nodes] = counts(&b.log);
        if kill == usize::MAX {
            // the whole old state was local: only the changed paths are fetched
            let asked: usize = b
                .log
                .iter()
                .map(|m| match m {
                    Message::GetTrieNodes(r) => r
                        .paths
                        .iter()
                        .map(|p| p.len().saturating_sub(1).max(1))
                        .sum(),
                    _ => 0,
                })
                .sum();
            assert!(
                nodes >= 1 && asked < total_nodes / 2,
                "heal asked for {asked} of {total_nodes} nodes"
            );
            println!("heal after a full sync of the old root: {nodes} requests, {asked} paths of {total_nodes} account-trie nodes");
            healed_any = true;
        }
    }
    assert!(healed_any);
}

/// xOneGo `2daf1e4b`: geth answers a storage request whose origin is past the account's last slot
/// with no slots and a boundary proof, and puts a proof on a last page that was not cut. The
/// client must read "more" from the proof check, and finish the account on the empty answer.
#[test]
fn a_storage_request_past_the_last_slot_completes_the_account() {
    let (flat, codes) = flat_state(300);
    let want = flat_root(&flat);
    let mut peer = Loopback::new(server(&flat, &codes));
    let mut store = StateStore::new(want);
    sync(&mut store, &mut peer, &small()).unwrap();
    let big = keccak256(addr(0));
    let last = *store.storage[&big].keys().next_back().unwrap();
    // the empty answer, as the server gives it
    let root = peer.server.snapshot().root();
    let Message::StorageRanges(r) = peer
        .server
        .respond(&Message::GetStorageRanges(GetStorageRanges {
            id: 1,
            root,
            accounts: vec![big],
            origin: B256::from(U256::from_be_bytes(last.0) + U256::from(1u8))
                .0
                .to_vec()
                .into(),
            limit: Bytes::new(),
            bytes: 4096,
        }))
        .unwrap()
    else {
        panic!()
    };
    assert!(
        r.slots.is_empty() && !r.proof.is_empty(),
        "no slots, a boundary proof"
    );
    // resume the account from there: it is done, and the state is unchanged
    store.progress.storage_next.insert(
        big,
        Some(B256::from(U256::from_be_bytes(last.0) + U256::from(1u8))),
    );
    store.progress.verified = None;
    let before = store.clone();
    sync(&mut store, &mut peer, &small()).unwrap();
    assert_eq!(store.progress.storage_next[&big], None);
    assert_eq!(store.accounts, before.accounts);
    assert_eq!(store.storage, before.storage);
    assert_eq!(store.progress.verified, Some(want));
}

#[test]
fn server_enforces_the_q15_bounds() {
    let (flat, codes) = flat_state(300);
    let mut s = server(&flat, &codes);
    let p = |i: u8| B512::repeat_byte(i);
    // 4 sessions in all, 1 per peer
    for i in 1..=4 {
        assert_eq!(s.admit(p(i), 0), Ok(()));
    }
    assert_eq!(s.admit(p(5), 0), Err(Reject::Sessions));
    assert_eq!(s.admit(p(1), 0), Err(Reject::PerPeer));
    s.release(p(1), 0);
    assert_eq!(s.admit(p(1), 0), Ok(()));
    for i in 1..=4 {
        s.release(p(i), 0);
    }
    // 16 MiB/s: a response that uses the second's bytes closes the door until time passes
    assert_eq!(s.admit(p(1), 10), Ok(()));
    s.release(p(1), 16 * 1024 * 1024);
    assert_eq!(s.admit(p(1), 10), Err(Reject::Rate));
    assert_eq!(s.admit(p(1), 110), Ok(())); // 100 ms refills 1.6 MiB
    s.release(p(1), 0);

    // a rejected request is answered empty, with its id; a peer that goes on is disconnected
    let mut s = server(&flat, &codes);
    let root = s.snapshot().root();
    let req = Message::GetAccountRange(GetAccountRange {
        id: 77,
        root,
        origin: B256::ZERO,
        limit: B256::repeat_byte(0xff),
        bytes: 1024,
    });
    assert!(s.admit(p(9), 0).is_ok()); // a session of that peer is open
    for _ in 0..3 {
        match s.handle(p(9), 0, &req) {
            Handled::RateExceeded {
                reply: Message::AccountRange(r),
                why: Reject::PerPeer,
            } => {
                assert_eq!((r.id, r.accounts.len(), r.proof.len()), (77, 0, 0));
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(s.handle(p(9), 0, &req), Handled::Disconnect);
    // an answer is not a request
    let resp = Message::ByteCodes(ByteCodes {
        id: 1,
        codes: vec![],
    });
    assert_eq!(s.handle(p(8), 0, &resp), Handled::Disconnect);
    s.release(p(9), 0);
    assert!(matches!(
        s.handle(p(9), 1000, &req),
        Handled::Reply(Message::AccountRange(_))
    ));
}

#[test]
fn responses_are_bounded() {
    let (flat, codes) = flat_state(3_000);
    let s = server(&flat, &codes);
    let root = s.snapshot().root();
    let max = B256::repeat_byte(0xff);
    // bytes = u64::MAX is cut to 2 MiB: the answer is at most 2 MiB plus the item that crossed it
    let Message::AccountRange(r) = s
        .respond(&Message::GetAccountRange(GetAccountRange {
            id: 1,
            root,
            origin: B256::ZERO,
            limit: max,
            bytes: u64::MAX,
        }))
        .unwrap()
    else {
        panic!()
    };
    let body: usize = r.accounts.iter().map(|a| 32 + a.body.len()).sum();
    assert_eq!(r.accounts.len(), 3_000); // the whole state is under 2 MiB
    assert!(body < 2 * 1024 * 1024);
    // 4 KiB: stops at the first account past the budget, and proves both edges
    let Message::AccountRange(r) = s
        .respond(&Message::GetAccountRange(GetAccountRange {
            id: 1,
            root,
            origin: B256::ZERO,
            limit: max,
            bytes: 4096,
        }))
        .unwrap()
    else {
        panic!()
    };
    let body: usize = r.accounts.iter().map(|a| 32 + a.body.len()).sum();
    assert!(body > 4096 && body < 4096 + 32 + 120, "{body}");
    assert!(!r.proof.is_empty());
    // storage: the slack is 10 %, then it stops and proves the cut
    let big = keccak256(addr(0));
    let Message::StorageRanges(r) = s
        .respond(&Message::GetStorageRanges(GetStorageRanges {
            id: 1,
            root,
            accounts: vec![big],
            origin: Bytes::new(),
            limit: Bytes::new(),
            bytes: 10_000,
        }))
        .unwrap()
    else {
        panic!()
    };
    let size: usize = r.slots[0].iter().map(|x| 32 + x.body.len()).sum();
    assert!((11_000..11_000 + 100).contains(&size), "{size}"); // stops once past the 10 % slack
    assert!(!r.proof.is_empty());
    // codes: 1,024 lookups at most, and bytes
    let hashes: Vec<B256> = codes.keys().copied().cycle().take(5_000).collect();
    let Message::ByteCodes(r) = s
        .respond(&Message::GetByteCodes(GetByteCodes {
            id: 1,
            hashes,
            bytes: u64::MAX,
        }))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(r.codes.len(), 1024);
    // trie nodes: 1,024 lookups at most
    let paths = vec![vec![Bytes::from_static(&[0x00])]; 5_000];
    let Message::TrieNodes(r) = s
        .respond(&Message::GetTrieNodes(GetTrieNodes {
            id: 1,
            root,
            paths,
            bytes: u64::MAX,
        }))
        .unwrap()
    else {
        panic!()
    };
    assert!(r.nodes.len() <= 1025 && !r.nodes.is_empty());
    // an empty path set is a protocol violation; an unknown root, an empty answer
    assert!(s
        .respond(&Message::GetTrieNodes(GetTrieNodes {
            id: 1,
            root,
            paths: vec![vec![]],
            bytes: 1
        }))
        .is_err());
    let Message::AccountRange(r) = s
        .respond(&Message::GetAccountRange(GetAccountRange {
            id: 1,
            root: B256::repeat_byte(9),
            origin: B256::ZERO,
            limit: max,
            bytes: 1,
        }))
        .unwrap()
    else {
        panic!()
    };
    assert!(r.accounts.is_empty() && r.proof.is_empty());
}

/// A peer that corrupts one kind of answer. The driver must refuse it, store nothing from it,
/// and give up after its failure budget.
struct Tamper {
    inner: Loopback,
    f: Box<dyn FnMut(Message) -> Message>,
    faults: usize,
}

impl PeerSource for Tamper {
    fn exchange(&mut self, req: Message) -> Result<Message, PeerError> {
        Ok((self.f)(self.inner.exchange(req)?))
    }
    fn report(&mut self, _: &Fault) {
        self.faults += 1;
    }
}

fn tampered(
    f: impl FnMut(Message) -> Message + 'static,
    run: impl FnOnce(&mut StateStore, &mut Tamper) -> SyncError,
) -> (StateStore, SyncError, usize) {
    let (flat, codes) = flat_state(1_500);
    let mut store = StateStore::new(flat_root(&flat));
    let mut t = Tamper {
        inner: Loopback::new(server(&flat, &codes)),
        f: Box::new(f),
        faults: 0,
    };
    let e = run(&mut store, &mut t);
    (store, e, t.faults)
}

fn must_refuse(f: impl FnMut(Message) -> Message + 'static) -> (StateStore, SyncError) {
    let (s, e, faults) = tampered(f, |s, t| sync(s, t, &small()).unwrap_err());
    assert_eq!(faults, 8, "every attempt is reported to the source");
    (s, e)
}

#[test]
fn a_range_without_its_proof_is_never_accepted() {
    // the proof stripped: a partial range cannot hash to the root
    let (store, e) = must_refuse(|m| match m {
        Message::AccountRange(mut r) => {
            r.proof.clear();
            Message::AccountRange(r)
        }
        m => m,
    });
    assert_eq!(e, SyncError::Fault(Fault::Proof(ProofError::Mismatch)));
    assert!(
        store.accounts.is_empty(),
        "nothing is stored from a refused range"
    );
    // a range that starts later and has no proof is refused as such: the first range is honest
    let (_, e, _) = tampered(
        |m| m,
        |s, t| {
            let mut first = true;
            t.f = Box::new(move |m| match m {
                Message::AccountRange(mut r) if !first => {
                    r.proof.clear();
                    Message::AccountRange(r)
                }
                Message::AccountRange(r) => {
                    first = false;
                    Message::AccountRange(r)
                }
                m => m,
            });
            sync(s, t, &small()).unwrap_err()
        },
    );
    assert_eq!(e, SyncError::Fault(Fault::Proof(ProofError::NoProof)));
}

#[test]
fn corrupted_answers_are_refused_and_not_stored() {
    type Mut = fn(Message) -> Message;
    let cases: Vec<(&str, Mut)> = vec![
        ("a leaf dropped", |m| match m {
            Message::AccountRange(mut r) => {
                r.accounts.remove(3);
                Message::AccountRange(r)
            }
            m => m,
        }),
        ("a leaf changed", |m| match m {
            Message::AccountRange(mut r) => {
                let mut b = r.accounts[2].body.to_vec();
                b[1] ^= 1;
                r.accounts[2].body = b.into();
                Message::AccountRange(r)
            }
            m => m,
        }),
        ("two leaves swapped", |m| match m {
            Message::AccountRange(mut r) => {
                r.accounts.swap(1, 2);
                Message::AccountRange(r)
            }
            m => m,
        }),
        ("a leaf added", |m| match m {
            Message::AccountRange(mut r) => {
                let mut a = r.accounts.last().cloned().unwrap();
                a.hash = B256::repeat_byte(0xfe);
                r.accounts.push(a);
                Message::AccountRange(r)
            }
            m => m,
        }),
        ("proof truncated", |m| match m {
            Message::AccountRange(mut r) => {
                r.proof.pop();
                Message::AccountRange(r)
            }
            m => m,
        }),
        ("proof replaced by noise", |m| match m {
            Message::AccountRange(mut r) => {
                r.proof = vec![Bytes::from_static(&[0xc0]), Bytes::from_static(&[0x80; 40])];
                Message::AccountRange(r)
            }
            m => m,
        }),
        ("wrong request id", |m| match m {
            Message::AccountRange(mut r) => {
                r.id += 1;
                Message::AccountRange(r)
            }
            m => m,
        }),
        ("wrong message kind", |m| match m {
            Message::AccountRange(r) => Message::ByteCodes(ByteCodes {
                id: r.id,
                codes: vec![],
            }),
            m => m,
        }),
    ];
    for (name, f) in cases {
        let (store, e) = must_refuse(f);
        assert!(matches!(e, SyncError::Fault(_)), "{name}: {e:?}");
        assert!(
            store.accounts.is_empty(),
            "{name}: stored data from a refused range"
        );
    }
}

#[test]
fn corrupted_storage_code_and_node_answers_are_refused() {
    let tamper_after_accounts = |f: fn(Message) -> Message| {
        let (_, e, faults) = tampered(f, |s, t| sync(s, t, &small()).unwrap_err());
        assert_eq!(faults, 8);
        e
    };
    // storage: a partial range without its proof, a changed slot value
    let e = tamper_after_accounts(|m| match m {
        Message::StorageRanges(mut r) => {
            r.proof.clear();
            Message::StorageRanges(r)
        }
        m => m,
    });
    assert!(matches!(e, SyncError::Fault(Fault::Proof(_))), "{e:?}");
    let e = tamper_after_accounts(|m| match m {
        Message::StorageRanges(mut r) => {
            if let Some(s) = r.slots.get_mut(0).and_then(|s| s.get_mut(0)) {
                s.body = alloy_rlp::encode(U256::from(0xdeadu64)).into();
            }
            Message::StorageRanges(r)
        }
        m => m,
    });
    assert!(matches!(e, SyncError::Fault(Fault::Proof(_))), "{e:?}");
    // code that does not hash to what was asked
    let e = tamper_after_accounts(|m| match m {
        Message::ByteCodes(mut r) => {
            if let Some(c) = r.codes.get_mut(0) {
                *c = Bytes::from_static(b"not the code");
            }
            Message::ByteCodes(r)
        }
        m => m,
    });
    assert_eq!(
        e,
        SyncError::Fault(Fault::Bad("code that was not requested"))
    );
}

#[test]
fn a_stale_pivot_is_reported_and_nothing_is_called_complete() {
    let (flat, codes) = flat_state(500);
    // a peer that has the state of some other root
    let mut peer = Loopback::new(server(&flat, &codes));
    let mut store = StateStore::new(B256::repeat_byte(7));
    let e = sync(&mut store, &mut peer, &small()).unwrap_err();
    assert_eq!(e, SyncError::Unavailable);
    assert_eq!(store.progress.verified, None);
}

// ---- completeness (the link to xone_sync::bootstrap::pivot_plan is in conformance/snap)

#[derive(Default)]
struct Held {
    have: BTreeSet<(Kind, u64)>,
    /// Kinds the peer can serve.
    serves: BTreeSet<Kind>,
    fetched: Vec<(Kind, u64, u64)>,
}

impl History for Held {
    fn has(&self, k: Kind, n: u64) -> bool {
        self.have.contains(&(k, n))
    }
    fn fetch(&mut self, k: Kind, a: u64, b: u64) -> Result<(), PeerError> {
        self.fetched.push((k, a, b));
        if self.serves.contains(&k) {
            self.have.extend((a..=b).map(|n| (k, n)));
            Ok(())
        } else {
            Err(PeerError::Unavailable)
        }
    }
}

#[test]
fn completeness_refuses_missing_pre_pivot_data() {
    let (flat, codes) = flat_state(200);
    let root = flat_root(&flat);
    let mut store = StateStore::new(root);
    sync(
        &mut store,
        &mut Loopback::new(server(&flat, &codes)),
        &Config::default(),
    )
    .unwrap();
    let pivot = 1_000;
    let needs = [
        Need {
            kind: Kind::Header,
            first: 900,
            last: pivot,
        },
        Need {
            kind: Kind::Body,
            first: 950,
            last: pivot,
        },
        Need {
            kind: Kind::Receipt,
            first: 990,
            last: pivot,
        },
        Need {
            kind: Kind::State,
            first: pivot,
            last: pivot,
        },
    ];
    // the state is done, the bodies and receipts are not and the peer cannot serve them
    let mut held = Held::default();
    held.have.extend((900..=pivot).map(|n| (Kind::Header, n)));
    held.serves.insert(Kind::Header);
    let e = finish(&store, root, &needs, &mut held).unwrap_err();
    assert_eq!(
        e,
        SyncError::Incomplete(Incomplete(vec![
            Need {
                kind: Kind::Body,
                first: 950,
                last: pivot
            },
            Need {
                kind: Kind::Receipt,
                first: 990,
                last: pivot
            },
        ]))
    );
    assert_eq!(
        held.fetched.len(),
        2,
        "they were asked for before the refusal"
    );
    // a hole in the middle is reported as that hole only
    let mut held = Held::default();
    held.have.extend(
        (900..=pivot)
            .filter(|n| !(930..=935).contains(n))
            .map(|n| (Kind::Header, n)),
    );
    held.have.extend((950..=pivot).map(|n| (Kind::Body, n)));
    held.have.extend((990..=pivot).map(|n| (Kind::Receipt, n)));
    let e = finish(&store, root, &needs, &mut held).unwrap_err();
    assert_eq!(
        e,
        SyncError::Incomplete(Incomplete(vec![Need {
            kind: Kind::Header,
            first: 930,
            last: 935
        }]))
    );
    // the same hole, fetched: complete
    held.serves.insert(Kind::Header);
    assert_eq!(finish(&store, root, &needs, &mut held), Ok(()));
    // the state of another root, or a state that was never verified, is not the pivot state
    assert!(
        matches!(finish(&store, B256::repeat_byte(1), &needs, &mut held), Err(SyncError::Incomplete(Incomplete(v))) if v.len() == 1 && v[0].kind == Kind::State)
    );
    let fresh = StateStore::new(root);
    assert!(finish(&fresh, root, &needs, &mut held).is_err());
}

// ---- no panics on hostile input

mod hostile {
    use super::*;
    use proptest::prelude::*;
    use xone_snap::proof::verify_range;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn decode_never_panics(code in 0u64..10, body in proptest::collection::vec(any::<u8>(), 0..400)) {
            let _ = Message::decode(code, &body);
        }

        #[test]
        fn decode_of_a_mutated_message_never_panics(pos in 0usize..4096, bit in 0u8..8, which in 0usize..4) {
            let msg = match which {
                0 => Message::AccountRange(AccountRange { id: 1, accounts: vec![AccountData { hash: B256::ZERO, body: Bytes::from_static(&[0xc4, 0x01, 0x01, 0x80, 0x80]) }], proof: vec![Bytes::from_static(&[1, 2, 3])] }),
                1 => Message::StorageRanges(StorageRanges { id: 1, slots: vec![vec![StorageData { hash: B256::ZERO, body: Bytes::from_static(&[1]) }]], proof: vec![] }),
                2 => Message::GetTrieNodes(GetTrieNodes { id: 1, root: B256::ZERO, paths: vec![vec![Bytes::from_static(&[0])]], bytes: 9 }),
                _ => Message::GetStorageRanges(GetStorageRanges { id: 1, root: B256::ZERO, accounts: vec![B256::ZERO], origin: Bytes::new(), limit: Bytes::new(), bytes: 1 }),
            };
            let code = msg.code();
            let mut b = msg.encode();
            let i = pos % b.len();
            b[i] ^= 1 << bit;
            let _ = Message::decode(code, &b);
        }
    }

    /// One bit flipped in one node of a valid proof: refused or, if the bit was in a part the check
    /// does not read, accepted; never a panic.
    #[test]
    fn verify_range_never_panics_on_a_mutated_proof() {
        let (flat, _) = flat_state(400);
        let leaves: BTreeMap<B256, Vec<u8>> = flat
            .accounts()
            .map(|(a, mut acc)| {
                acc.storage_root = xone_state::trie::storage_root(flat.slots(a));
                (keccak256(a), acc.rlp())
            })
            .collect();
        let trie = Trie::build(&leaves);
        let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(2_000));
        runner
            .run(
                &(0usize..64, 0usize..600, 0u8..8, 0usize..390, 0usize..12),
                |(node, pos, bit, from, len)| {
                    let keys: Vec<B256> = leaves.keys().copied().skip(from).take(len).collect();
                    let values: Vec<Bytes> =
                        keys.iter().map(|k| leaves[k].clone().into()).collect();
                    let origin = keys.first().copied().unwrap_or(B256::ZERO);
                    let mut proof = trie.prove(origin);
                    if let Some(l) = keys.last() {
                        proof.extend(trie.prove(*l));
                    }
                    let n = node % proof.len();
                    let mut b = proof[n].to_vec();
                    let i = pos % b.len();
                    b[i] ^= 1 << bit;
                    proof[n] = b.into();
                    let _ = verify_range(trie.root, origin, &keys, &values, &proof);
                    Ok(())
                },
            )
            .unwrap();
    }
}

#[test]
fn one_account_and_its_storage_are_fetched_with_proofs() {
    let (flat, codes) = flat_state(500);
    let mut peer = Loopback::new(server(&flat, &codes));
    let root = peer.server.snapshot().root();
    // account 0 has 3,000 slots: several proven storage ranges
    let h0 = keccak256(addr(0));
    let (acc, slots) = account_at(&mut peer, &small(), root, h0).unwrap().unwrap();
    assert_eq!(slots.len(), 3000);
    assert_eq!(acc.nonce, 0);
    let want: std::collections::BTreeMap<B256, U256> = flat
        .slots(addr(0))
        .map(|(s, v)| (keccak256(s), v))
        .collect();
    assert_eq!(slots, want);
    // an absent account is proven absent, not an error
    assert_eq!(
        account_at(&mut peer, &small(), root, B256::repeat_byte(0x42)).unwrap(),
        None
    );
    // a root the peer does not hold is Unavailable
    assert_eq!(
        account_at(&mut peer, &small(), B256::repeat_byte(1), h0),
        Err(SyncError::Unavailable)
    );
}

#[test]
fn preimages_of_hashed_rows_are_served() {
    use xone_snap::preimage::GetPreimages;
    let (flat, codes) = flat_state(50);
    let accounts: Vec<_> = flat.hashed_accounts().collect();
    let storage: Vec<_> = accounts
        .iter()
        .flat_map(|(h, _)| flat.hashed_slots(*h).map(move |(s, v)| (*h, s, v)))
        .collect();
    let mut pre = std::collections::HashMap::new();
    for (a, _) in flat.accounts() {
        pre.insert(keccak256(a), Bytes::from(a.to_vec()));
        for (s, _) in flat.slots(a) {
            pre.insert(keccak256(s), Bytes::from(s.to_vec()));
        }
    }
    let snap = Snapshot::from_rows(&accounts, &storage, codes.clone()).with_preimages(pre);
    assert_eq!(snap.root(), Snapshot::from_flat(&flat, codes).root());
    let srv = Server::new(snap, Limits::default());
    let a = addr(10);
    let (s, _) = flat.slots(a).next().unwrap();
    let r = srv.preimages(&GetPreimages {
        id: 3,
        hashes: vec![keccak256(a), B256::repeat_byte(9), keccak256(s)],
    });
    assert_eq!(r.id, 3);
    let want: Vec<Bytes> = vec![a.0.to_vec().into(), s.0.to_vec().into()];
    assert_eq!(r.preimages, want);
}

/// Apothem, 2026-11-13, geth `5929639e`: a large contract's storage range proof carried a branch
/// whose child right of the range is an embedded node (a leaf with a one-byte value 9 nibbles
/// deep, 31 bytes of RLP, so inside its parent). The check answered `Shape` for it, from both geth
/// peers, 28 minutes into the download. Geth accepts it, and so does a range that has the embedded
/// leaf on its left.
#[test]
fn an_embedded_leaf_beside_the_range_is_part_of_its_parent() {
    use alloy_primitives::keccak256;
    use xone_snap::{mpt::Trie, proof::verify_range};
    let at = |prefix: &str, nib: char, rest: &str| {
        let mut s = format!("{prefix}{nib}{rest}");
        s.truncate(64);
        B256::from_slice(&alloy_primitives::hex::decode(s).unwrap())
    };
    let p = "27eea59d"; // 8 shared nibbles, then a branch whose children 4 and e are leaves of 55 nibbles
    let rest = "f8ec01c2f235c80824dc64cee4311b24a4bfb06c47ae8dfeabb26c2";
    let (small, other) = (at(p, '4', rest), at(p, 'e', rest));
    let mut leaves = BTreeMap::new();
    for i in 0..400u32 {
        leaves.insert(keccak256(i.to_be_bytes()), vec![0x02]);
    }
    leaves.insert(small, vec![0x02]);
    leaves.insert(other, vec![0x02]);
    let t = Trie::build(&leaves);
    let all: Vec<(B256, Bytes)> = leaves
        .iter()
        .map(|(k, v)| (*k, Bytes::from(v.clone())))
        .collect();
    let range = |lo: usize, hi: usize| {
        let (k, v): (Vec<B256>, Vec<Bytes>) = all[lo..=hi].iter().cloned().unzip();
        let mut proof = t.prove(k[0]);
        for n in t.prove(*k.last().unwrap()) {
            if !proof.contains(&n) {
                proof.push(n);
            }
        }
        verify_range(t.root, k[0], &k, &v, &proof)
    };
    let i = all.iter().position(|(k, _)| *k == small).unwrap();
    assert_eq!(all[i + 1].0, other);
    // the embedded leaf is right of the range, then left of it
    assert_eq!(range(i - 20, i), Ok(true), "right of the range");
    assert_eq!(range(i + 1, i + 20), Ok(true), "the leaf is the first key");
    assert_eq!(range(i, i), Ok(true), "right of a one-key range");
    assert_eq!(range(i + 2, i + 30), Ok(true), "left of the range");
}
