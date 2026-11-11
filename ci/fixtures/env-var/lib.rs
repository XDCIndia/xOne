// expect: std::env::var
pub fn f() -> Option<String> {
    std::env::var("HOME").ok()
}
