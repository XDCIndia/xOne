// expect: -D clippy::unused-result-ok
pub fn f(r: Result<u8, ()>) {
    r.ok();
}
