#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: the vectors are trusted inputs and a panic is the failure signal"
)]
//! Rules of the pool on hand-built transactions (no signatures: the sender is a field). The oracle scenarios
//! cover the same rules against geth; these name them one by one and fix the clock.
mod common;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use common::{account, MapState};
use std::sync::Arc;
use xone_builder::txpool::{
    propagate::{Intake, Q5_PEER_TXS, Q6_TXS},
    special::{Authority, SIGN_SELECTOR},
    Config, Error, Pool, PoolTx, Status,
};
use xone_chainspec::Network;
use xone_executor::tx::Tx;

const HEAD: u64 = 85_000_000; // Apothem: Prague, 625 gwei tier
const TIER: u128 = 625_000_000_000;
const SIGNERS: Address = address!("0000000000000000000000000000000000000089");
const NOW: u64 = 1_700_000_000;

fn who(i: u8) -> Address {
    Address::repeat_byte(i)
}

fn tx(from: Address, nonce: u64, to: Address, price: u128, data: Vec<u8>) -> Arc<PoolTx> {
    let mut h = [0u8; 32];
    h[..20].copy_from_slice(from.as_slice());
    h[20..28].copy_from_slice(&nonce.to_be_bytes());
    h[28..].copy_from_slice(&u32::try_from(price % 1_000_000_007).unwrap().to_be_bytes());
    Arc::new(PoolTx {
        hash: B256::from(h),
        size: 120 + u64::try_from(data.len()).unwrap(),
        tx: Tx {
            ty: 0,
            chain_id: Some(51),
            nonce,
            gas_price: price,
            priority_fee: None,
            gas: 200_000,
            to: Some(to),
            value: U256::ZERO,
            data: Bytes::from(data),
            access_list: vec![],
            from,
        },
    })
}

fn sign_data(block: u64) -> Vec<u8> {
    let mut d = SIGN_SELECTOR.to_vec();
    d.extend_from_slice(&U256::from(block).to_be_bytes::<32>());
    d.extend_from_slice(&[7u8; 32]);
    d
}

fn rich(addrs: &[Address]) -> MapState {
    let mut s = MapState::default();
    for a in addrs {
        s.0.insert(*a, account(0, U256::from(10u64).pow(U256::from(24)), &[]));
    }
    s
}

fn pool(cfg: &Config, signers: &[Address]) -> Pool {
    Pool::new(
        Network::Apothem.spec(),
        cfg,
        Authority::candidates(signers.iter().copied()),
        HEAD,
        50_000_000,
    )
    .unwrap()
}

fn one(p: &mut Pool, s: &MapState, t: &Arc<PoolTx>) -> Result<(), Error> {
    p.add_batch(s, NOW, std::slice::from_ref(t)).remove(0)
}

#[test]
fn a_sign_tx_from_a_candidate_is_promoted_and_one_from_anyone_else_is_refused() {
    let (cand, other) = (who(1), who(2));
    let s = rich(&[cand, other]);
    let mut p = pool(&Config::default(), &[cand]);
    let ok = tx(cand, 0, SIGNERS, 0, sign_data(HEAD - 5));
    assert_eq!(one(&mut p, &s, &ok), Ok(()));
    assert_eq!(
        p.status(&ok.hash),
        Status::Pending,
        "promoted straight to pending"
    );
    let bad = tx(other, 0, SIGNERS, 0, sign_data(HEAD - 5));
    assert!(matches!(one(&mut p, &s, &bad), Err(Error::SpecialTxNotFromSigner(a)) if a == other));
    assert!(!p.has(&bad.hash));
    // While syncing nobody is a candidate.
    p.set_authority(Authority::Syncing);
    let again = tx(cand, 1, SIGNERS, 0, sign_data(HEAD - 4));
    assert_eq!(
        one(&mut p, &s, &again).unwrap_err().name(),
        "special transaction sender is not a signer"
    );
}

#[test]
fn a_special_tx_skips_the_base_fee_floor_and_a_regular_one_below_it_is_refused() {
    let (cand, user) = (who(1), who(2));
    let s = rich(&[cand, user]);
    let mut p = pool(&Config::default(), &[cand]);
    let free = tx(cand, 0, SIGNERS, 0, sign_data(HEAD - 5));
    assert_eq!(
        one(&mut p, &s, &free),
        Ok(()),
        "price 0, below the 625 gwei floor"
    );
    let low = tx(user, 0, who(9), TIER - 1, vec![]);
    assert_eq!(
        one(&mut p, &s, &low).unwrap_err().name(),
        "transaction gas price under xdc tier minimum"
    );
    let at = tx(user, 0, who(9), TIER, vec![]);
    assert_eq!(one(&mut p, &s, &at), Ok(()));
    // The packer's tip filter does not cut a special transaction either (legacypool.go:543-547).
    let pending = p.pending(Some(1), TIER, None);
    assert_eq!(pending[&cand].len(), 1);
    assert!(
        !pending.contains_key(&user),
        "tip at the base fee is zero, below the minimum"
    );
}

#[test]
fn a_candidate_pool_with_penalised_members_authorises_all_of_them() {
    let (masternode, penalised, standby, stranger) = (who(1), who(2), who(3), who(4));
    let s = rich(&[masternode, penalised, standby, stranger]);
    let mut p = pool(&Config::default(), &[masternode, penalised, standby]);
    for (i, a) in [masternode, penalised, standby, stranger]
        .into_iter()
        .enumerate()
    {
        let r = one(
            &mut p,
            &s,
            &tx(
                a,
                0,
                SIGNERS,
                0,
                sign_data(HEAD - 5 - u64::try_from(i).unwrap()),
            ),
        );
        assert_eq!(r.is_ok(), a != stranger, "{a}");
    }
}

#[test]
fn known_calldata_is_exactly_the_sign_shape_and_a_gapped_sign_tx_queues() {
    let cand = who(1);
    let s = rich(&[cand]);
    let mut p = pool(&Config::default(), &[cand]);
    // A gapped nonce is queued, not promoted, even for a candidate with the right calldata.
    let gapped = tx(cand, 3, SIGNERS, 0, sign_data(HEAD - 5));
    assert_eq!(one(&mut p, &s, &gapped), Ok(()));
    assert_eq!(p.status(&gapped.hash), Status::Queued);
    // 69 bytes is not the sign shape (the oracle scenario `special-bad-length-full-pool` shows a full pool
    // refusing it).
    let mut long = sign_data(HEAD - 5);
    long.push(0);
    let t = tx(cand, 0, SIGNERS, 0, long);
    assert!(!xone_builder::txpool::special::known_calldata(&t));
    assert!(xone_builder::txpool::special::known_calldata(&tx(
        cand,
        0,
        SIGNERS,
        0,
        sign_data(1)
    )));
}

#[test]
fn balance_above_2_to_256_is_judged_by_its_low_256_bits_as_geth_does() {
    let a = who(1);
    let mut s = MapState::default();
    // 2^256 + 5: wide, and BALANCE sees 5.
    let wide = xone_primitives::WideBalance::from_be_bytes(&{
        let mut b = vec![1u8];
        b.extend_from_slice(&[0u8; 31]);
        b.push(5);
        b
    });
    assert!(wide.is_wide());
    s.0.insert(
        a,
        xone_state::Account {
            balance: wide,
            ..Default::default()
        },
    );
    let mut p = pool(&Config::default(), &[]);
    // cost 200,000 * 625 gwei is far above 5
    assert_eq!(
        one(&mut p, &s, &tx(a, 0, who(9), TIER, vec![]))
            .unwrap_err()
            .name(),
        "insufficient funds for gas * price + value"
    );
}

#[test]
fn queued_accounts_expire_after_the_lifetime_on_a_fixed_clock_and_pending_ones_never_do() {
    let (a, b) = (who(1), who(2));
    let s = rich(&[a, b]);
    let cfg = Config::default();
    assert_eq!(
        cfg.lifetime_secs,
        3 * 60 * 60,
        "geth DefaultConfig.Lifetime"
    );
    let mut p = pool(&cfg, &[]);
    let gapped = tx(a, 5, who(9), TIER, vec![]);
    let live = tx(b, 0, who(9), TIER + 1, vec![]);
    one(&mut p, &s, &gapped).unwrap();
    one(&mut p, &s, &live).unwrap();
    assert_eq!(p.stats(), (1, 1));
    assert_eq!(
        p.evict_expired(&s, NOW + cfg.lifetime_secs),
        0,
        "idle for exactly the lifetime is not past it"
    );
    assert_eq!(p.evict_expired(&s, NOW + cfg.lifetime_secs + 1), 1);
    assert_eq!(p.stats(), (1, 0));
    assert_eq!(
        p.evict_expired(&s, NOW + 1000 * cfg.lifetime_secs),
        0,
        "an executable transaction does not age out"
    );
    assert!(p.has(&live.hash));
}

#[test]
fn a_replacement_needs_the_price_bump_and_the_account_bounds_hold() {
    let a = who(1);
    let s = rich(&[a, who(2), who(3), who(4)]);
    let cfg = Config {
        account_queue: 2,
        global_queue: 3,
        ..Config::default()
    };
    let mut p = pool(&cfg, &[]);
    let t0 = tx(a, 0, who(9), TIER + 1000, vec![]);
    assert_eq!(one(&mut p, &s, &t0), Ok(()));
    let t1 = tx(a, 0, who(9), TIER + 1000 + (TIER + 1000) / 10 - 1, vec![]);
    assert_eq!(one(&mut p, &s, &t1), Err(Error::ReplaceUnderpriced));
    let t2 = tx(a, 0, who(9), TIER + 1000 + (TIER + 1000) / 10, vec![]);
    assert_eq!(one(&mut p, &s, &t2), Ok(()));
    // per-account queue: nonces 5..9 gapped, only two stay
    for n in 5..10 {
        let _ = one(
            &mut p,
            &s,
            &tx(who(2), n, who(9), TIER + 10 + n as u128, vec![]),
        );
    }
    assert_eq!(p.content().1[&who(2)].len(), 2, "AccountQueue");
    // global queue: three queued in all
    for n in 5..8 {
        let _ = one(
            &mut p,
            &s,
            &tx(who(3), n, who(9), TIER + 100 + n as u128, vec![]),
        );
        let _ = one(
            &mut p,
            &s,
            &tx(who(4), n, who(9), TIER + 200 + n as u128, vec![]),
        );
    }
    assert!(p.stats().1 <= 3, "GlobalQueue: {}", p.stats().1);
}

#[test]
fn a_nonce_gap_stays_queued_until_the_gap_is_filled() {
    let a = who(1);
    let s = rich(&[a]);
    let mut p = pool(&Config::default(), &[]);
    let late = tx(a, 2, who(9), TIER + 1, vec![]);
    one(&mut p, &s, &late).unwrap();
    assert_eq!(p.status(&late.hash), Status::Queued);
    one(&mut p, &s, &tx(a, 0, who(9), TIER + 2, vec![])).unwrap();
    assert_eq!(
        p.status(&late.hash),
        Status::Queued,
        "nonce 1 is still missing"
    );
    one(&mut p, &s, &tx(a, 1, who(9), TIER + 3, vec![])).unwrap();
    assert_eq!(p.status(&late.hash), Status::Pending);
    assert_eq!(p.nonce(&s, a), 3);
}

#[test]
fn intake_rejects_past_the_adr_007_bounds_without_scoring() {
    let mut i = Intake::default();
    let mut ok = 0;
    for _ in 0..(Q5_PEER_TXS + 10) {
        ok += usize::from(i.admit(1, 200));
    }
    assert_eq!(ok, Q5_PEER_TXS, "Q5: 128 transactions per peer");
    let mut ok = 0;
    for peer in 2..100 {
        for _ in 0..Q5_PEER_TXS {
            ok += usize::from(i.admit(peer, 200));
        }
    }
    assert_eq!(ok + Q5_PEER_TXS, Q6_TXS, "Q6: 4,096 transactions in all");
    for _ in 0..Q5_PEER_TXS {
        i.done(1, 200);
    }
    assert!(
        i.admit(500, 200),
        "room again after the first peer's items left admission"
    );
    let mut j = Intake::default();
    assert!(!j.admit(1, 1024 * 1024 + 1), "a single item over 1 MiB");
    assert!(j.admit(1, 1024 * 1024) && !j.admit(1, 1), "1 MiB per peer");
}

#[test]
fn the_slot_cap_refuses_a_cheaper_tx_when_full_and_evicts_the_cheapest_for_a_dearer_one() {
    let cfg = Config {
        global_slots: 4,
        global_queue: 2,
        ..Config::default()
    };
    let addrs: Vec<Address> = (1..=8).map(who).collect();
    let s = rich(&addrs);
    let mut p = pool(&cfg, &[]);
    for (i, a) in addrs.iter().take(6).enumerate() {
        one(
            &mut p,
            &s,
            &tx(*a, 0, who(99), TIER + 100 + 10 * i as u128, vec![]),
        )
        .unwrap();
    }
    assert_eq!(p.len().1, 6);
    assert_eq!(
        one(&mut p, &s, &tx(addrs[6], 0, who(99), TIER + 50, vec![])),
        Err(Error::Underpriced)
    );
    one(&mut p, &s, &tx(addrs[7], 0, who(99), TIER + 500, vec![])).unwrap();
    assert_eq!(p.len().1, 6, "one in, the cheapest out");
}
