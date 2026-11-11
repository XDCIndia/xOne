// expect: -F unsafe-code
static mut COUNTER: u32 = 0;
pub fn bump() {
    unsafe {
        COUNTER += 1;
    }
}
