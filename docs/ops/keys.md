# Keys: operator guide

Crate: `xone-signer` (issue #40). Specs: `spec/keys/keystore-v3.md`, `spec/keys/node-key.md`. Status: the key readers and the in-process signer exist; the node does not call them yet, so the paths below are the contract, not a running feature.

## Which keys

| Key | File | Format | Used for |
|---|---|---|---|
| Validator (producer) key | a geth keystore v3 file, for example `<datadir>/keystore/UTC--...` | encrypted JSON (scrypt or pbkdf2) | signing votes, timeouts, seals. Only the V2 safety unit may call it. |
| Password | a password file you choose, for example `<datadir>/miner.password` | plain text, first line is the password (a trailing `\r` is dropped) | unlocking the keystore at start |
| Node key | `<datadir>/nodekey` | 64 hex characters | devp2p identity (the enode id) |

Use the keystore that `geth account new` (or the XDC tooling) wrote. xOne reads scrypt light, scrypt standard and pbkdf2 files. It does not read version 1 or presale files; convert them with geth first.

## Permissions

- Keystore file, password file and `nodekey`: mode **0600** or narrower (0400 is fine). Owner only.
- The data directory: 0700. xOne creates a missing directory with 0700.
- A file with any group or other bit, or any execute bit, is **refused** with a `Permissions` error that names the file and the mode. Fix it with `chmod 600 <file>`.
- `AllowInsecurePermissions` is an explicit override in the calling code. It is not a command-line flag or an environment variable and there is none planned. When it is used, xOne logs a `warn` line with the path and mode on every load. Use it only to get a node up while you fix the mode, never in production.
- xOne does not check who owns the file or the mode of the directory above it. Keep the keystore directory 0700 yourself.

## What xOne never does

- It never writes a private key, a password or a decrypted key to a log, an error message, a panic message, an RPC response or a metric. Tests sign 1,000 digests at trace level and format every error path, and fail if the key appears raw, in hex, or in base64.
- It never accepts a key or password on the command line or in an environment variable (so it cannot show in `ps` or `/proc`).
- It never silently replaces a node key. If `nodekey` exists but cannot be read or is malformed, the node stops with an error and leaves the file alone. A new `nodekey` is created only when the file is missing. (geth creates a new one after any read error; the node identity then changes without notice.)
- It never writes a validator key. It only reads a keystore you made.
- It never leaves key or password bytes in memory after use on purpose: they are zeroized when dropped. An OS that swaps the process or writes a core dump is outside what the program can prevent; disable swap and core dumps on a validator host.
- It never signs through the remote-signer interface with a fencing token older than one the signer has already seen: the request is refused (`StaleFence`).

## Errors you may see

| Error | Meaning | Action |
|---|---|---|
| `WrongPassword` | the MAC did not match | check the password file (first line only, no extra spaces); the keystore file may also be damaged |
| `Malformed` | not keystore JSON, wrong version, bad hex, key not 32 bytes, address field differs from the key, or a KDF cost above the cap | re-export the key with geth; do not edit the file |
| `UnsupportedKdf` / `UnsupportedCipher` | the file uses something other than scrypt, pbkdf2/hmac-sha256, aes-128-ctr | re-encrypt with geth |
| `Permissions` | mode wider than 0600 | `chmod 600 <file>` |
| `Io` | open or read failed (the kind is shown, not the content) | check the path and the owner |

## Not covered yet

The remote-signer network transport, HSM support, and the call from the safety unit are later items. A seat move between hosts is still the manual sequence: stop the old signer, confirm it is gone, then start the new one (ADR-010 section 8).
