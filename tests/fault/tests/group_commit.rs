#![allow(clippy::unwrap_used, clippy::expect_used, clippy::as_conversions)]
//! Group commit (`xone_sync::mdbx::Config::group_commit`, a node that does not sign): a `kill -9` loses
//! nothing, and a power loss goes back to the last steady point, whose blocks read back whole.

mod support;
use std::time::Duration;

use support::{crash, *};
use xone_db::{
    fault::{MemDir, Pick},
    Cause, Durability,
};
use xone_fault::{anchored, cfg, copy_dir, Scratch};
use xone_sync::store::Store;

#[test]
fn a_kill_loses_nothing_and_a_power_loss_returns_to_the_last_steady_point() {
    let p = Parts::new(false, 3, ["s1", "s2", "s3", "m4", "m5"], 4);
    let scratch = Scratch::new("group");
    let mut c = cfg(p.retention, p.bpf);
    c.group_commit = Some(Duration::from_secs(3600));
    let g = &p.t.blocks["s0"];
    let chain = p.chain(anchored(
        &scratch,
        MemDir::new(),
        g.block.header.clone(),
        Default::default(),
        c,
    ));
    for l in ["s1", "s2", "s3"] {
        chain.insert(p.t.block(l), 0).unwrap();
    }
    // deferred blocks: no signing on them
    assert_eq!(
        chain.with_store(|s| s.durability()),
        Durability::Degraded(Cause::GroupCommit)
    );
    chain.with_store_mut(|s| s.steady()).unwrap();
    assert_eq!(chain.with_store(|s| s.durability()), Durability::Durable);
    // what MDBX rolls back to after a crash of the system
    let steady = Scratch::new("group-steady");
    copy_dir(
        &scratch.path().join("chain.mdbx"),
        &steady.path().join("chain.mdbx"),
    );
    for l in ["m4", "m5"] {
        chain.insert(p.t.block(l), 0).unwrap();
    }
    let (path, dir) = chain.into_store().into_parts();

    // kill -9: the page cache holds every deferred write
    let killed = Scratch::new("group-kill");
    copy_dir(&path, &killed.path().join("chain.mdbx"));
    let (_, r) = crash::restart(&p, &killed, dir.crash(Pick::Everything));
    let (c, _) = r.unwrap();
    assert_eq!(label_of(&p.t, c.head().1), "m5");
    crash::assert_sound(c, &p.t);

    // power loss: only synced segment bytes and the steady MDBX commit survive
    let (_, r) = crash::restart(&p, &steady, dir.crash(Pick::Nothing));
    let (c, _) = r.unwrap();
    assert_eq!(label_of(&p.t, c.head().1), "s3");
    crash::assert_sound(c, &p.t);
}
