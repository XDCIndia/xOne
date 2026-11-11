// expect: core::option::Option::unwrap_or_default
pub fn f(r: Option<u8>) -> u8 {
    r.unwrap_or_default()
}
