# Keystore vectors

Every file in `keystore/` holds a keystore v3 JSON written for a **throwaway key** that was
generated at random for this test and is not used anywhere. The password in each file is also
throwaway. No real key, and no plaintext private key of any kind, is stored in this repository.

Source: `gen/main.go`, run against the pinned geth-XDC tree (`accounts/keystore`,
`XDCIndia/go-ethereum` at `6fc8e8c2`) with Go 1.25.2. `go.mod.txt` is the module file it was run
with (rename it to `go.mod`; its `replace` points at a local checkout of the pin).

| File | Made by | KDF |
|---|---|---|
| `scrypt-light.json` | `keystore.EncryptKey`, `LightScryptN`/`LightScryptP` (N=4096, p=6) | scrypt |
| `scrypt-standard.json` | `keystore.EncryptKey`, `StandardScryptN`/`StandardScryptP` (N=262144, p=1) | scrypt |
| `pbkdf2.json` | geth has no pbkdf2 encryptor; the generator builds the JSON the way `EncryptDataV3` does with `c=262144`, `prf=hmac-sha256`, then checks that the pinned `keystore.DecryptKey` accepts it | pbkdf2 |
| `scrypt-light-xdc-address.json` | `scrypt-light.json` with the `address` field rewritten to `xdc<hex>`; the pinned `DecryptKey` was checked to accept it | scrypt |

geth-XDC's `EncryptKey` writes `address` as bare hex (`accounts/keystore/passphrase.go:184-198`),
so it never writes an `xdc` prefix. It does read one: `common.FromHex` accepts `xdc`
(`common/bytes.go:29-34`) and the account cache parses the field with it
(`accounts/keystore/account_cache.go:268`). The fourth file is therefore edited, not written by
geth; the field is not covered by the MAC.

Each file is `{name, password, address, keystore}`; `address` is the expected 20-byte address in hex.
