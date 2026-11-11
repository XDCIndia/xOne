// expect: std::sync::OnceLock
static G: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
pub fn f() -> Option<&'static u8> {
    G.get()
}
