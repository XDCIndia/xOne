// expect: -D clippy::unwrap-used
pub fn f(r: Result<u8, ()>) -> u8 {
    r.unwrap()
}
