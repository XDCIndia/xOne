#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! Fork choice against geth. `vectors/fork_choice.json` is written by `oracle/` from the pinned
//! geth-XDC `core.BlockChain`: per scenario and event, the head and the canonical chain geth chose.
//! The tests build the same tree with synthetic blocks (real state roots from a model) and require
//! the same head, the same canonical hashes and the same refusals. Includes the 50 generated
//! sibling pairs and the committed-block floor scenarios.

mod common;
use common::*;
use std::collections::BTreeSet;
use xone_sync::chain::{
    fork_choice::{decide, Choice, Facts},
    ChainRead, InsertError, ReorgError,
};

/// Every label a scenario names, with the shared prefix.
fn labels(s: &Scenario) -> BTreeSet<String> {
    let mut l: BTreeSet<String> = (1..=s.fork).map(|n| format!("s{n}")).collect();
    l.extend((s.fork + 1..=s.main).map(|n| format!("m{n}")));
    l.extend((s.fork + 1..=s.alt).map(|n| format!("a{n}")));
    l
}

/// Runs one scenario; returns the number of events checked.
fn run(s: &Scenario) -> usize {
    let mut w = World::new(s.mode == "v2", s.fork);
    for l in labels(s) {
        w.make(&l);
    }
    let store = w.genesis_store(64, 4);
    let (chain, t) = w.into_chain(store);
    for n in 1..=s.fork {
        chain.insert(t.block(&format!("s{n}")), 0).unwrap();
    }
    for l in s.setup.iter().flatten() {
        chain.insert(t.block(l), 0).unwrap();
    }
    if !s.committed.is_empty() {
        // The 3-chain rule on consecutive rounds put the floor where the oracle was told it is.
        assert_eq!(
            chain.committed().unwrap().hash,
            t.hash(&s.committed),
            "{}",
            s.name
        );
    }
    for (i, ev) in s.events.iter().enumerate() {
        let what = format!("{} event {i} {}", s.name, ev.block);
        let r = chain.insert(t.block(&ev.block), 0);
        match ev.err.as_str() {
            "" => {
                r.unwrap_or_else(|e| panic!("{what}: {e}"));
            }
            "stop reorg" => assert!(
                matches!(
                    r,
                    Err(InsertError::Reorg(ReorgError::BelowCommitted { .. }))
                ),
                "{what}: {r:?}"
            ),
            other => panic!("{what}: oracle error {other}"),
        }
        let (hn, hh) = chain.head();
        assert_eq!(hh, t.hash(&ev.head), "{what}: head");
        assert_eq!(hn, ev.canonical.len() as u64, "{what}");
        for (n, l) in ev.canonical.iter().enumerate() {
            assert_eq!(
                chain.reader().canonical_hash(n as u64 + 1),
                Some(t.hash(l)),
                "{what}: #{}",
                n + 1
            );
        }
        assert_eq!(
            chain.reader().canonical_hash(hn + 1),
            None,
            "{what}: index above head"
        );
        t.assert_state(&chain, &ev.head);
    }
    let head = s
        .events
        .last()
        .map_or_else(|| format!("s{}", s.fork), |e| e.head.clone());
    let store = chain.into_store();
    assert_eq!(
        store.flat_root(),
        t.blocks[&head].block.header.state_root,
        "{}: flat state root",
        s.name
    );
    s.events.len()
}

#[test]
fn every_scenario_equals_geth() {
    let all = scenarios();
    let mut events = 0;
    for s in &all {
        events += run(s);
    }
    let gen = all.iter().filter(|s| s.name.starts_with("gen/")).count();
    let floor = all.iter().filter(|s| s.name.starts_with("floor/")).count();
    let shape = all.iter().filter(|s| s.name.starts_with("shape/")).count();
    assert_eq!((gen, floor, shape), (50, 4, 8));
    println!(
        "fork choice vs geth: {} scenarios, {events} events, 0 mismatches",
        all.len()
    );
}

/// The pure function on the facts of every event of every scenario: the choice it names is the
/// move geth made (the head changed to the block, or it did not).
#[test]
fn decide_matches_every_geth_event() {
    // Replays only fork choice: tracks (head label number, head hash label) from the oracle itself.
    let mut checked = 0;
    for s in scenarios() {
        let w = World::new(s.mode == "v2", s.fork);
        let mut head = format!("s{}", s.fork);
        let mut head_n = s.fork;
        let known = |h: &str| h.to_owned();
        for l in s.setup.iter().flatten() {
            head = known(l);
            head_n = l[1..].parse().unwrap();
        }
        for ev in &s.events {
            let n: u64 = ev.block[1..].parse().unwrap();
            let parent_is_head = w.parent_label(&ev.block) == head;
            let c = decide(Facts {
                v2: s.mode == "v2",
                parent_is_head,
                number: n,
                head_number: head_n,
            });
            let moved = ev.head == ev.block;
            match c {
                Choice::Extend => assert!(moved, "{} {}", s.name, ev.block),
                Choice::Side => assert!(!moved, "{} {}", s.name, ev.block),
                // A reorg to a block moves the head unless the floor refuses it.
                Choice::Reorg => assert_eq!(moved, ev.err.is_empty(), "{} {}", s.name, ev.block),
            }
            if moved {
                head = ev.block.clone();
                head_n = n;
            }
            checked += 1;
        }
    }
    assert!(checked > 200);
}

/// Mutants of the rule. Each is run against the same events and must disagree with geth somewhere:
/// the vectors are able to see these bugs.
#[test]
fn mutants_of_the_rule_are_caught() {
    type Rule = fn(Facts) -> Choice;
    // xOneGo d4cdf136, be3a8403: equal-height sibling decisions.
    let adopt_equal: Rule = |f| match decide(f) {
        Choice::Side if f.number == f.head_number => Choice::Reorg,
        c => c,
    };
    let never_side: Rule = |f| {
        if f.parent_is_head {
            Choice::Extend
        } else {
            Choice::Reorg
        }
    };
    let keep_lower_v1: Rule = |f| match decide(f) {
        Choice::Reorg if f.number <= f.head_number => Choice::Side,
        c => c,
    };
    for (name, rule) in [
        ("adopt-equal", adopt_equal),
        ("never-side", never_side),
        ("keep-lower-v1", keep_lower_v1),
    ] {
        let mut diffs = 0;
        for s in scenarios() {
            let w = World::new(s.mode == "v2", s.fork);
            let (mut head, mut head_n) = (format!("s{}", s.fork), s.fork);
            for l in s.setup.iter().flatten() {
                (head, head_n) = (l.clone(), l[1..].parse().unwrap());
            }
            for ev in &s.events {
                let n: u64 = ev.block[1..].parse().unwrap();
                let f = Facts {
                    v2: s.mode == "v2",
                    parent_is_head: w.parent_label(&ev.block) == head,
                    number: n,
                    head_number: head_n,
                };
                let moved = ev.head == ev.block;
                let says_move = rule(f) != Choice::Side && ev.err.is_empty();
                diffs += usize::from(says_move != moved);
                if moved {
                    (head, head_n) = (ev.block.clone(), n);
                }
            }
        }
        assert!(diffs > 0, "mutant {name} agrees with geth on every vector");
    }
}
