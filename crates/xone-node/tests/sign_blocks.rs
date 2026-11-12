#![allow(clippy::unwrap_used, clippy::expect_used)] // integration test: a panic is the failure report
//! The producer signs every sign block imported since the last head it saw, not only the tip (issue #132; xOneTS
//! `packages/node/test/gate-d.test.ts` test 15, defect 15 of the TS Gate D rehearsal).

mod common;

use common::{parts, scratch, vectors};
use xone_chainspec::Network;
use xone_node::services::{
    core::{now, Core},
    producer::skipped_sign_blocks,
};

/// TS test 15 (soak, blocks 5280 and 6810): a sign block and its child were imported within one producer tick,
/// the producer saw only the child as its head and never signed the sign block. geth signs on every chain-head
/// event of an imported block (`miner/xdpos_worker.go:231-246`, `maybeBroadcastImportedSignTx` `:1119`). On
/// Apothem (TIP2019 at 1) the sign blocks are the multiples of 15.
#[test]
fn a_sign_block_imported_with_its_child_is_signed_on_the_childs_head() {
    let net = Network::Apothem;
    let spec = net.spec();
    let dir = scratch("sign-blocks");
    let core = Core::open(spec.clone(), &net.genesis().unwrap(), &dir.join("xone"), 0).unwrap();
    let blocks = vectors(46);
    let import = |range: std::ops::RangeInclusive<usize>| {
        for b in &blocks[range] {
            let (h, body) = parts(b);
            core.import(h, body, now()).unwrap();
        }
    };
    // the producer saw head 14; sign block 15 and its child 16 arrive before its next tick
    import(1..=14);
    import(15..=16);
    {
        let h = core.hist.read();
        assert_eq!(
            skipped_sign_blocks(&spec, &h, Some(14), 16),
            [(15, blocks[15].header.hash())],
            "the sign block under the head is owed its sign transaction"
        );
        assert!(
            skipped_sign_blocks(&spec, &h, Some(15), 16).is_empty(),
            "15 was a head itself: signed then"
        );
        assert!(
            skipped_sign_blocks(&spec, &h, None, 16).is_empty(),
            "the first head after a start: the tip only, a restart does not sign again"
        );
    }
    // geth's freshness window (`isSigningTxStale`, `miner/xdpos_worker.go:1192-1209`): 30 blocks below the head
    import(17..=46);
    let h = core.hist.read();
    assert_eq!(
        skipped_sign_blocks(&spec, &h, Some(0), 46)
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>(),
        [30, 45],
        "15 is 31 below the head: stale"
    );
    assert!(
        skipped_sign_blocks(&spec, &h, Some(46), 40).is_empty(),
        "a head below the last one (an unwind): nothing skipped"
    );
    drop(h);
    drop(core);
    let _ = std::fs::remove_dir_all(&dir);
}
