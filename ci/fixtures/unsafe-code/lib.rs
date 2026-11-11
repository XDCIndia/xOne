// expect: -F unsafe-code
pub fn read(p: *const u8) -> u8 {
    unsafe { *p }
}
