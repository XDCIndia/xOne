// expect: std::sync::LazyLock
static G: std::sync::LazyLock<u8> = std::sync::LazyLock::new(|| 1);
pub fn f() -> u8 {
    *G
}
