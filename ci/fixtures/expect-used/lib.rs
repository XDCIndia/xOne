// expect: -D clippy::expect-used
pub fn f(r: Option<u8>) -> u8 {
    r.expect("present")
}
