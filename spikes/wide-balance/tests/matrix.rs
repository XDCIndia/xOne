//! The wide-balance matrix: every row of vectors/matrix.json (printed by the geth-XDC Go
//! oracle) must give the same balances, storage, tx outcome and state root on revm.
//!
//! A row that differs is not weakened: it is listed in `KNOWN_DIVERGENT` with its cause, and
//! the test fails if that set changes in either direction.

use wide_balance_spike::{
    harness::{diff, rows, run},
    journal::Carry,
};

/// Option A only. `AccountInfo::is_empty` (revm-state 43.0.1 account_info.rs:316-321) also
/// requires an empty extension, so a wide account with nonce 0, no code and a wrapped part of
/// zero is not empty to revm. geth `stateObject.empty()` (state_object.go:92-94) tests the
/// wrapped balance only, so geth deletes it at EIP-161 finalisation and revm keeps it.
#[cfg(feature = "account-ext")]
const KNOWN_DIVERGENT_A: &[&str] = &["wide_low_zero_empty.legacy"];

fn failing(carry: Carry) -> Vec<(String, String)> {
    let all = rows();
    assert_eq!(all.len(), 40);
    all.iter()
        .filter_map(|r| {
            let d = diff(r, &run(r, carry));
            (!d.is_empty()).then(|| (r.id.clone(), d.join("; ")))
        })
        .collect()
}

#[test]
fn option_b_side_map() {
    let bad = failing(Carry::Side);
    assert!(bad.is_empty(), "{bad:#?}");
}

#[cfg(feature = "account-ext")]
#[test]
fn option_a_account_ext() {
    let bad = failing(Carry::Ext);
    let ids: Vec<&str> = bad.iter().map(|(i, _)| i.as_str()).collect();
    assert_eq!(ids, KNOWN_DIVERGENT_A, "{bad:#?}");
}
