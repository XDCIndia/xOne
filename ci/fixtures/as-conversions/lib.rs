// expect: -D clippy::as-conversions
pub fn f(x: u64) -> u32 {
    x as u32
}
