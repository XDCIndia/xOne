# Keystore-v3 intake under hostile input (issue #61)

The happy path and the format are in `keystore-v3.md`. This page is the failure contract of
`xone_signer::local_keystore::{load, decrypt}` for a file that is not trusted.

## Order (each step runs only if the one before passed)

1. Bounded read: 64 KiB, regular file, mode 0600 (`secure_file.rs`). A larger byte slice given to
   `decrypt` is refused the same way (`ResourcePolicy`).
2. Parse with typed fields (`serde`): wrong JSON type, negative, fractional, exponent or above-`u64`
   numbers, missing required fields and duplicate keys (`crypto` with `Crypto` included) are
   `Malformed`. Unknown extra keys are ignored, as in geth.
3. `version == 3`, `cipher == aes-128-ctr`, KDF `scrypt` or `pbkdf2` with `prf == hmac-sha256`.
4. Shapes before any derivation: salt 1..64 bytes, ciphertext 32, IV 16, MAC 32, `dklen` 32..128.
5. Cost before any derivation: scrypt `N` a power of two >= 2, `r, p >= 1`, `p <= 64`,
   `128 * r * N <= 1 GiB` (checked multiply); pbkdf2 `1 <= c <= 2^24`. Above the policy is
   `ResourcePolicy`: re-encrypt the key offline with geth's standard or light parameters. The policy
   is never relaxed for one file.
6. KDF into a zeroizing buffer; MAC compared in constant time (no early exit); a mismatch is
   `WrongPassword`.
7. AES-128-CTR into a zeroizing buffer; scalar `1 <= d < n` (k256 `SigningKey::from_slice`);
   present `address` field equal to the derived address.
8. Only then is a `LocalSigner` built. Every error returns before it, so no partial signer, no
   signing history and no log of a secret exist on failure (`leak.rs` scans logs and panics).

Residual copies: a 32-byte key moved on the stack can leave a stale copy that zeroize cannot reach
(`secret.rs`). The derived key and plaintext are `Zeroizing` and are cleared on every path.

## Evidence

- `crates/xone-signer/tests/keystore_intake.rs`: 63 hostile cases in one test binary, each with its
  typed class, under 2 s, with no password or key bytes in the error text, and peak RSS growth
  under 64 MiB for the whole matrix (Linux `VmHWM`): no case reaches a KDF buffer above policy.
  IV 0/15/17 bytes, ciphertext 0/31/33, MAC 31/33, scalar 0, n, n+1 and 2^256-1, wrong and
  non-hex address, all KDF number forms.
- `crates/xone-signer/tests/keys.rs` `hostile_keystores_are_refused_before_any_derivation` (the
  first slice, PR #111).
- Fuzz: `fuzz/fuzz_targets/keystore_decrypt.rs` drives `decrypt` with any bytes and any password.

  ```
  python3 -I fuzz/keystore_seeds.py <seed-dir>   # generated keystores: random fields, cheap costs, no real key
  cd fuzz && cargo +nightly fuzz run keystore_decrypt <seed-dir> -- -max_total_time=600 -timeout=30
  ```

  Corpora and artifacts are not committed
  (`fuzz/.gitignore`); a saved corpus holds generated data only.
