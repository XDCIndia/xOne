// expect: std::env::var_os
pub fn f() -> bool {
    std::env::var_os("HOME").is_some()
}
