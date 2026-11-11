//! Per-transfer cost of the adapters on narrow accounts (the common case).
//! Run: cargo test --release -p wide-balance-spike --test perf -- --ignored --nocapture

use alloy_primitives::{Address, U256};
use revm::{context::Journal, context_interface::journaled_state::JournalTr, state::AccountInfo};
use std::time::Instant;
use wide_balance_spike::{
    harness::{Mem, MemAcct},
    journal::{Carry, WideJournal},
};

fn mem() -> Mem {
    let mut m = Mem::default();
    for i in [1u8, 2] {
        m.accounts.insert(
            Address::repeat_byte(i),
            MemAcct {
                info: AccountInfo {
                    balance: U256::from(1_000_000_000u64),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
    }
    m
}

fn run<J: JournalTr<Database = Mem>>(mut j: J) -> f64 {
    let (a, b) = (Address::repeat_byte(1), Address::repeat_byte(2));
    j.load_account(a).unwrap();
    j.load_account(b).unwrap();
    let n = 2_000_000u32;
    let t = Instant::now();
    for i in 0..n {
        let (f, to) = if i % 2 == 0 { (a, b) } else { (b, a) };
        assert!(j.transfer_loaded(f, to, U256::from(1)).is_none());
        if i % 1024 == 0 {
            j.commit_tx();
            j.load_account(a).unwrap();
            j.load_account(b).unwrap();
        }
    }
    t.elapsed().as_nanos() as f64 / n as f64
}

#[test]
#[ignore]
fn narrow_transfer_cost() {
    println!(
        "stock revm Journal      : {:7.1} ns/transfer",
        run(<Journal<Mem> as JournalTr>::new(mem()))
    );
    println!(
        "WideJournal option B    : {:7.1} ns/transfer",
        run(WideJournal::new(mem()).with_carry(Carry::Side))
    );
    #[cfg(feature = "account-ext")]
    println!(
        "WideJournal option A    : {:7.1} ns/transfer",
        run(WideJournal::new(mem()).with_carry(Carry::Ext))
    );
    #[cfg(feature = "account-ext")]
    println!(
        "AccountInfo size (ext)  : {} bytes",
        std::mem::size_of::<AccountInfo>()
    );
    #[cfg(not(feature = "account-ext"))]
    println!(
        "AccountInfo size        : {} bytes",
        std::mem::size_of::<AccountInfo>()
    );
}
