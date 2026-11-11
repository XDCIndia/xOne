# Node key file (`nodekey`) in geth-XDC

Pin: `XDCIndia/go-ethereum` at `6fc8e8c2088b7f18264f5663036669d04fa7becf`. Rust: `crates/xone-signer/src/nodekey.rs`.

## Rule

1. The devp2p identity key is a secp256k1 scalar stored as **64 hex characters** (32 bytes), no `0x`, no newline written.
2. Location: `<datadir>/<name>/nodekey`. `name` is the instance name (the executable name unless set); an old geth layout `<datadir>/nodekey` is used if present.
3. Read (`LoadECDSA`): the first 64 bytes must be printable ASCII hex; a byte below `!` before 64 characters is "too short". After the 64 characters only `\r` or `\n` may follow, at most two of them (a third is "too long"). The value must be a valid scalar (not zero, below `N`).
4. If no datadir is set the key is ephemeral and never written.
5. If the file cannot be read for **any** reason, geth generates a new key, creates the instance directory with mode 0700 and writes the file with mode 0600 (`os.WriteFile`, `SaveECDSA`). The node identity changes silently.
6. `--nodekey <file>` loads a file with `LoadECDSA` and exits on error; `--nodekeyhex` takes the hex on the command line (testing only); both together is an error.
7. The node id (enode) is the 64-byte uncompressed public key without the `0x04` prefix.

## Source

- `node/config.go:36` (`datadirPrivateKey = "nodekey"`), `:334-343` (`name`), `:345-378` (`ResolvePath`, old-layout rule), `:380-385` (`instanceDir`), `:387-422` (`NodeKey`: ephemeral `:397-402`, load `:404-407`, generate and persist `:409-420`)
- `crypto/crypto.go:172-192` (`LoadECDSA`), `:196-207` (`readASCII`), `:210-224` (`checkKeyFileEnd`), `:228-231` (`SaveECDSA`, mode 0600), `:106-128` (`toECDSA`)
- `cmd/utils/flags.go:980-989` (flags), `:1285-1306` (`setNodeKey`)

## Where xOne differs on purpose

- **No silent regeneration.** xOne creates a key only when the file does not exist (`NotFound`). A file that exists but is unreadable, malformed, or too wide in mode is an error and the file is left untouched. (geth `node/config.go:404-406` regenerates on every error; xOneGo regenerates on a parse failure.) A changed node identity is a visible event, so it needs the operator.
- **Mode check.** A `nodekey` with bits outside 0600 is refused unless `AllowInsecurePermissions` is passed (logged at `warn`). geth does not check.
- **Creation.** The key is written to a 0600 temp file in the same directory, synced, then hard-linked to `nodekey`; two racing creators produce one key. The datadir is created with mode 0700 if missing. The instance subdirectory (`name`) is the caller's choice: pass the directory that should hold `nodekey`.
- Not read: `--nodekeyhex` (a key on a command line is visible in `ps`).

## Networks

All.
