#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    reason = "test code"
)]
//! Head change with reinjection (geth `legacypool.go:1736-1871`, #63): a reorg drops two blocks and their
//! eligible transactions come back; a transaction the new branch includes and one the new state makes
//! invalid do not.

mod common;

use alloy_primitives::{Address, Bytes, B256, U256};
use common::{account, MapState};
use std::{collections::BTreeSet, sync::Arc};
use xone_builder::txpool::{
    pending::{Branch, Reorged, MAX_REORG_DEPTH},
    special::Authority,
    Config, Error, Pool, PoolTx, Status,
};
use xone_chainspec::Network;
use xone_executor::tx::Tx;

const HEAD: u64 = 85_000_000;
const TIER: u128 = 625_000_000_000;

fn who(i: u8) -> Address {
    Address::from([i; 20])
}

fn mk(id: u8, from: Address, nonce: u64) -> Arc<PoolTx> {
    Arc::new(PoolTx {
        hash: B256::from([id; 32]),
        size: 110,
        tx: Tx {
            ty: 0,
            chain_id: Some(51),
            nonce,
            gas_price: TIER,
            priority_fee: None,
            gas: 21_000,
            to: Some(who(0xee)),
            value: U256::ZERO,
            data: Bytes::new(),
            access_list: vec![],
            from,
        },
    })
}

fn state(nonces: &[(Address, u64, u64)]) -> MapState {
    let mut st = MapState::default();
    for (a, n, bal) in nonces {
        st.0.insert(
            *a,
            account(
                *n,
                U256::from(*bal) * U256::from(10u64).pow(U256::from(18)),
                &[],
            ),
        );
    }
    st
}

#[test]
fn reorg_of_two_blocks_reinjects_only_eligible_transactions() {
    let (a, b, c) = (who(1), who(2), who(3));
    // old branch: blocks HEAD+1, HEAD+2 hold a0, a1 (a is only on the old branch), b0 (also on the new
    // branch) and c0 (c is out of funds on the new head).
    let (a0, a1, b0, c0) = (mk(1, a, 0), mk(2, a, 1), mk(3, b, 0), mk(4, c, 0));
    let old = state(&[(a, 2, 100), (b, 1, 100), (c, 1, 100)]);
    let mut pool = Pool::new(
        Network::Apothem.spec(),
        &Config::default(),
        Authority::Syncing,
        HEAD + 2,
        50_000_000,
    )
    .unwrap();
    let _ = &old;
    // new head HEAD+2' : a is back at nonce 0, b included its tx elsewhere (nonce 1), c has no balance
    let new = state(&[(a, 0, 100), (b, 1, 100), (c, 0, 0)]);
    let branch = Branch {
        depth: 2,
        discarded: vec![a0.clone(), a1.clone(), b0.clone(), c0.clone()],
        included: BTreeSet::from([b0.hash]),
    };
    let out = pool
        .reset_reorg(&new, 1, HEAD + 2, 50_000_000, &branch)
        .unwrap();
    let Reorged::Reinjected(res) = out else {
        panic!("{out:?}")
    };
    // a0 and a1 admitted; b0 not offered (included elsewhere); c0 refused (no funds)
    assert_eq!(res.len(), 3);
    assert_eq!(res[0], Ok(()));
    assert_eq!(res[1], Ok(()));
    assert_eq!(res[2], Err(Error::InsufficientFunds));
    assert_eq!(pool.status(&a0.hash), Status::Pending);
    assert_eq!(pool.status(&a1.hash), Status::Pending);
    assert_eq!(pool.status(&b0.hash), Status::Unknown);
    assert_eq!(pool.status(&c0.hash), Status::Unknown);
    // the same notification again changes nothing
    let Reorged::Reinjected(again) = pool
        .reset_reorg(&new, 2, HEAD + 2, 50_000_000, &branch)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        again,
        vec![
            Err(Error::AlreadyKnown),
            Err(Error::AlreadyKnown),
            Err(Error::InsufficientFunds)
        ]
    );
    assert_eq!(pool.stats(), (2, 0));
}

#[test]
fn nonce_already_used_on_the_new_branch_is_not_reinjected() {
    let a = who(1);
    let a0 = mk(1, a, 0);
    let mut pool = Pool::new(
        Network::Apothem.spec(),
        &Config::default(),
        Authority::Syncing,
        HEAD + 1,
        50_000_000,
    )
    .unwrap();
    // the new branch spent nonce 0 of a with another transaction
    let new = state(&[(a, 1, 100)]);
    let branch = Branch {
        depth: 1,
        discarded: vec![a0.clone()],
        included: BTreeSet::from([B256::from([9; 32])]),
    };
    let Reorged::Reinjected(res) = pool
        .reset_reorg(&new, 1, HEAD + 1, 50_000_000, &branch)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(res, vec![Err(Error::NonceTooLow { next: 1, tx: 0 })]);
    assert!(pool.is_empty());
}

#[test]
fn deep_reorg_resets_without_reinjection() {
    let a = who(1);
    let mut pool = Pool::new(
        Network::Apothem.spec(),
        &Config::default(),
        Authority::Syncing,
        HEAD,
        50_000_000,
    )
    .unwrap();
    let new = state(&[(a, 0, 100)]);
    let branch = Branch {
        depth: MAX_REORG_DEPTH + 1,
        discarded: vec![mk(1, a, 0)],
        included: BTreeSet::new(),
    };
    let out = pool
        .reset_reorg(&new, 1, HEAD, 50_000_000, &branch)
        .unwrap();
    assert_eq!(
        out,
        Reorged::TooDeep {
            depth: MAX_REORG_DEPTH + 1
        }
    );
    assert!(pool.is_empty());
}
