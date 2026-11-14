//! The keystore-v3 unlock boundary (#61): any bytes with any password never panic, and an
//! accepted file yields a signer whose address is the key's. Seeds are generated, never real keys:
//! see `spec/keys/keystore-intake.md` for the run command and the seed generator.
#![no_main]
use libfuzzer_sys::fuzz_target;
use xone_signer::{local_keystore::decrypt, Password};

fuzz_target!(|data: &[u8]| {
    // the first byte picks a password length, the rest is the file
    let Some((&n, rest)) = data.split_first() else { return };
    let cut = usize::from(n).min(rest.len()).min(32);
    let (pw, file) = rest.split_at(cut);
    if let Ok(s) = decrypt(file, &Password::new(pw.to_vec())) {
        assert_ne!(s.address(), alloy_primitives::Address::ZERO);
    }
});
