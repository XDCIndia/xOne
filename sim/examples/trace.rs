//! `cargo run -p xone-sim --release --example trace -- <seed>`: one seed, every round, vote, timeout and commit.
fn main() {
    let seed: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("a seed");
    match xone_sim::net::run_traced(seed) {
        Ok(r) => eprintln!("{r:?}"),
        Err(e) => eprintln!("{e}"),
    }
}
