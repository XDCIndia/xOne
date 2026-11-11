# Keystore v3 (Web3 Secret Storage) as read by geth-XDC

Pin: `XDCIndia/go-ethereum` at `6fc8e8c2088b7f18264f5663036669d04fa7becf`. Rust: `crates/xone-signer/src/local_keystore.rs`; vectors in `crates/xone-signer/tests/vectors/`.

## Rule

1. The file is JSON with `version` 3, `crypto{cipher, ciphertext, cipherparams{iv}, kdf, kdfparams, mac}` and an informational `address`. Hex fields have no `0x` prefix.
2. `cipher` must be `aes-128-ctr`. Any other value is refused.
3. The KDF is `scrypt` (`n`, `r`, `p`, `dklen`, `salt`) or `pbkdf2` (`c`, `dklen`, `prf` = `hmac-sha256`, `salt`). Any other KDF, or another pbkdf2 PRF, is refused. The password is the raw bytes.
4. `derivedKey` is `dklen` bytes (32 for every file geth writes). `mac = keccak256(derivedKey[16:32] || ciphertext)`. It is compared before decryption; a mismatch is the "wrong password" error.
5. The plaintext is `AES-128-CTR(key = derivedKey[0:16], iv)` over the ciphertext, a 32-byte big-endian secp256k1 scalar. Length other than 32 bytes, zero, or at or above the curve order is refused.
6. The address is derived from the key (`keccak256(pubkey)[12:]`). geth does not compare it with the `address` field when decrypting.
7. geth writes `address` as bare hex. Its reader accepts `0x` and `xdc`/`XDC` prefixes (`FromHex`).
8. Parameters: light is `N=4096, r=8, p=6`; standard is `N=262144, r=8, p=1`; `dklen=32`; salt 32 bytes, iv 16 bytes.
9. Password file for non-interactive use: the first line, trailing `\r` removed.
10. geth never checks file permissions on read. It writes key files through `os.CreateTemp` (mode 0600) in a 0700 directory.

## Source

- `accounts/keystore/key.go:38` (`version = 3`), `:66-72` (`encryptedKeyJSONV3`), `:80-87` (`CryptoJSON`)
- `accounts/keystore/passphrase.go:50-68` (KDF constants), `:140-181` (`EncryptDataV3`: salt `:141`, iv `:151`, MAC `:159`, cipher name `:172`), `:184-198` (`EncryptKey`, bare-hex address)
- `accounts/keystore/passphrase.go:200-241` (`DecryptKey`), `:243-277` (`DecryptDataV3`: cipher check `:244`, MAC `:267`), `:279-293` (`decryptKeyV3`), `:333-359` (`getKDFKey`: pbkdf2 PRF `:349`)
- `accounts/keystore/presale.go:107-117` (`aesCTRXOR`, big-endian 128-bit counter)
- `accounts/keystore/key.go:190-199` (`writeTemporaryKeyFile`, dir 0700, temp file 0600)
- `accounts/keystore/account_cache.go:254-268` (address read with `HexToAddress`); `common/bytes.go:29-39` (`FromHex` accepts `0x` and `xdc`)
- `cmd/geth/accountcmd.go:224-238` (`readPasswordFromFile`)
- `crypto/crypto.go:106-128` (`toECDSA`: zero and `>= N` refused), `:253-256` (`PubkeyToAddress`)

## Where xOne differs on purpose

- **Address check.** xOne compares a present `address` field with the derived address and refuses a mismatch (`Malformed`). geth ignores it. xOneGo also checks it (`pkg/node/keystore_geth.go`, end of `loadGethKeystoreKey`). An absent or empty field is accepted.
- **KDF cost cap.** geth runs whatever `n`, `r`, `p`, `c` the file names. xOne refuses scrypt with `128*r*N` above 1 GiB, `N` not a power of two, `p > 64`, and pbkdf2 `c` above 2^24 (`Malformed`), before allocating. Geth standard needs 256 MiB.
- **Version 1 files** (`"version":"1"`, AES-CBC, `decryptKeyV1` at `passphrase.go:295`) and presale files are not read.
- **Permissions.** xOne refuses a key or password file with mode bits outside 0600 (`Permissions`), unless the caller passes `AllowInsecurePermissions`, which is logged at `warn`.
- **Key name** `Crypto` (capital C, written by some other tools) is accepted as an alias, as Go's JSON matching would.

## Vectors

`crates/xone-signer/tests/vectors/keystore/`: `scrypt-light`, `scrypt-standard`, `pbkdf2`, and `scrypt-light-xdc-address` (the `xdc` form of the field, edited by hand; geth-XDC itself writes bare hex). All four decrypt to the address geth printed; the first three also fail with `WrongPassword` on a wrong password.

## Networks

All. The key format does not depend on the chain.
