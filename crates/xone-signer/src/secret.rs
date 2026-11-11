//! Secret byte types. They zeroize on drop and never print their bytes.
//! A move of the 32-byte key can leave a stale copy on the stack; Rust cannot prevent that.

use std::fmt;

use zeroize::Zeroizing;

/// A 32-byte secp256k1 private scalar. No `Clone`, `Display` or byte-printing `Debug`.
pub struct KeyBytes(Zeroizing<[u8; 32]>);

impl KeyBytes {
    pub(crate) fn from_slice(b: &[u8]) -> Option<Self> {
        let mut k = Zeroizing::new([0u8; 32]);
        if b.len() != 32 {
            return None;
        }
        k.copy_from_slice(b);
        Some(Self(k))
    }

    pub(crate) fn zeroed() -> Self {
        Self(Zeroizing::new([0u8; 32]))
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) fn as_mut_bytes(&mut self) -> &mut [u8; 32] {
        &mut self.0
    }
}

impl fmt::Debug for KeyBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyBytes(<redacted>)")
    }
}

/// A keystore password. Zeroizes on drop, prints nothing.
pub struct Password(Zeroizing<Vec<u8>>);

impl Password {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}
