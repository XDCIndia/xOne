# snap/1 messages, framing, limits and the Q15 bounds

## Rule

snap/1 is a request and answer protocol next to eth on one RLPx connection. A request is an RLP list that starts with a request id; the answer carries the same id. Every message is encoded and decoded exactly as geth-XDC does, and decoding is strict: a non-canonical length or integer, a trailing byte, a hash that is not 32 bytes and a list above its cap are errors, not input to repair.

**Terms.** Codes are snap-relative; `xone-snap` knows only these. Slim account: `[nonce, balance, root-or-empty, code-or-empty]`.

### Capability and code offset

- The Hello lists `snap/1`. geth-XDC lists `snap/1` and `snap/2` when it is not itself syncing by snap or fast (`eth/backend.go:866-886`, `advertiseSnapV2`); the highest shared version is used, so xOne offers snap/1 only (snap/2 has no trie-node requests, no healing, and is a non-goal of XB-17).
- Offsets are assigned over the shared capabilities sorted by name, then version, from 16 (`p2p/peer.go:438-459` `matchProtocols`, `:43` `baseProtocolLength`). eth (100 or 101) is 229 messages (`eth/protocols/eth/protocol.go:89`), so snap code `c` is RLPx code `16 + 229 + c`. In `xone_net::rlpx::Conn` (which adds 16) that is eth-relative code `229 + c`; `conformance/snap/src/lib.rs` `SNAP_BASE` is this number.
- snap/1 has 8 messages (`eth/protocols/snap/protocol.go:45`); a code outside 0 to 7 is an error (`handler.go:193`). A response that is not asked for is a protocol error (`handlers.go:128-153` through `tracker.Fulfil`).
- geth handles snap only for a peer that finished the eth Status exchange. `conformance/snap` therefore runs Hello and Status before the first snap request. While a snap request is open the eth frames that arrive (gossip, votes) are read and dropped, so that they cannot block the answer (xOneGo `a3a92e02`, the head-of-line stall).

### The eight messages

| Code | Message | Body | Source |
| --- | --- | --- | --- |
| 0x00 | GetAccountRange | `[id, root, origin, limit, bytes]` | `protocol.go:51,77-83` |
| 0x01 | AccountRange | `[id, [[hash, slim-body]..], [proof-node..]]` | `:52,92-102` |
| 0x02 | GetStorageRanges | `[id, root, [account-hash..], origin, limit, bytes]`; `origin` and `limit` empty or 32 bytes | `:53,127-134` |
| 0x03 | StorageRanges | `[id, [[[slot-hash, body]..]..], [proof-node..]]`; the proof is for the last set only | `:54,136-153` |
| 0x04 | GetByteCodes | `[id, [code-hash..], bytes]` | `:55,174-178` |
| 0x05 | ByteCodes | `[id, [code..]]` | `:56,186-189` |
| 0x06 | GetTrieNodes | `[id, root, [[path..]..], bytes]` | `:57,192-197` |
| 0x07 | TrieNodes | `[id, [node..]]` | `:58,217-220` |

- The slim body is one RLP item, kept as received in the message and read by `Account::from_slim_rlp`. The storage body is the RLP of the trimmed 32-byte value, which is also the trie leaf; its string is wrapped once more as a byte string (`StorageData.Body []byte`, `protocol.go:150-153`).
- `GetStorageRanges.origin` and `limit` are `[]byte` in geth and are read with `common.BytesToHash` (`handlers.go:198,202`), which accepts any length. xOne accepts 0 or 32 bytes only (`CodecError::BadBound`): a length in between is a different request in geth (the hash is left-padded), and no client sends one.
- A path set is `[account path]` for the account trie or `[account hash, storage path..]` for one storage trie; paths are hex-prefix encoded without the terminator flag (`trie/encoding.go:37` `hexToCompact`, `:82` `compactToHex`, `trie/sync.go:71-102`). The empty path (the root) is `0x00`; the empty string also means the root. A flag byte with the terminator bit, or an odd flag with a filler, is refused (no node).

### The wide balance

The balance item of the slim account is a plain RLP big integer of any length: `SlimAccountRLPInto` writes `BigBalance` when it is set (`core/types/state_account.go:98-125`), and `FullAccount` reads the item as `big.Int` and keeps the true value (`:129-170`). So an account above 2^256 goes through AccountRange in full, and the item can be 33 bytes or more. `Account::slim_rlp` and `Account::from_slim_rlp` do the same (`xone-state`, spec/state/account-leaf.md); the oracle vectors cover 2^256-1, 2^256, 2^256+1, the two Apothem accounts, the maximum observed (258 bits), 2^264-1 and 2^264 (`spikes/wide-balance/REPORT.md`). The legal range `0 <= balance < 2^264` is an xOne rule: the client calls `check_legal` on every proven leaf and stops with `Fault::Balance`; the codec accepts any length, as geth does.

geth reads the root and code hash of a slim account as free-length byte strings. `Account::from_slim_rlp` refuses a length other than 0 or 32 (`SlimError::BadHashLength`) instead of padding or cutting it.

### Caps on what is received

| Cap | Value | Source |
| --- | --- | --- |
| Message body | 10 MiB | `protocol.go:48`, `handler.go:162` |
| Proof nodes in an answer | 128 | `handlers.go:128`, `:301` |
| Items of a list in an answer | 131,072 (xOne rule) | geth bounds by bytes only; 10 MiB of empty strings would be 10 M allocations. A 2 MiB answer holds at most 2 MiB / 37 B = 56,000 accounts |
| Items of a list in a request | 4,096 (xOne rule) | geth clients send at most 1,024 (`sync.go:66-72`, `:1328`) and servers serve at most 1,024 (`handler.go:36,45`) |
| Nesting of an account body | 4 | an account is one list of four strings |

## The server

`server::Server` answers from a `Snapshot`: the accounts and storage by hashed key, the account trie and every storage trie with all their nodes, and the code table. It is built from the flat tables (`Snapshot::from_flat`, `from_provider`) or from hashed state (`from_hashed`, what a snap-synced node holds). Only the state it was built from is served; any other root is answered empty (geth serves the layers of its snapshot tree, `handlers.go:62-64`). A node with no snapshot has no `Snapshot` and so no `Server`: it must not advertise snap/1. There is no stub that answers empty (XDCIndia/xOneGo#733).

### Answers (geth-XDC line for each rule)

- **Soft limit.** `bytes` is cut to 2 MiB (`handler.go:32`, `handlers.go:57,173,349,428`).
- **GetAccountRange** (`handlers.go:56-119`): accounts from `origin` in hash order; each adds `32 + len(body)`; stops after the first account with `hash >= limit` (that account is included, `:97`) or when `size > bytes` (after adding, `:100`); proof is `Prove(origin)` then `Prove(last)` when `last` is not zero (`:105-116`), nodes once each, in first-seen order. An unknown root gives no accounts and no proof.
- **GetStorageRanges** (`handlers.go:172-292`): accounts in order; stops before an account when `size >= bytes` (`:192`); only the first account uses `origin` and `limit` (`:196-203`); a slot is added while `size < bytes * 1.1` (`:228`; integer `bytes + bytes/10`, `stateLookupSlack` `handler.go:41`); the last slot of a range is the first with `hash >= limit` (`:244`). A proof is added, and the answer ends, when `origin` is not zero or the range was cut with slots served (`:257-290`). An account that is not in the state makes the whole answer empty (`:218-220`, the iterator fails).
- **GetByteCodes** (`handlers.go:348-374`): at most 1,024 hashes (`handler.go:36`); the empty code hash is answered with an empty string and costs nothing; an unknown code is skipped (no placeholder); stops after the code that takes the total over `bytes`.
- **GetTrieNodes** (`handlers.go:426-537`): a path set with no item is a protocol violation and ends the connection (`:465`, `:402-404`); an account path set returns one node per path; a storage path set returns the nodes of that account's trie, or nothing when the account has no storage trie; a path with no node gives an empty string (`trie/trie.go:301-305` returns no blob and no error, `handlers.go:478,522` append it). It stops when `bytes` is passed or more than 1,024 lookups were made (`:526,:532`). geth also stops at 5 s (`handler.go:54`); there is no clock in `xone-snap`, so the lookup count is the only work bound.

Verified against the live geth-XDC: nine requests (three account ranges with edge proofs, whole and chunked storage ranges, byte codes with the empty hash, trie nodes with an absent path) get answers equal, message for message, to geth's over the same state (`conformance/snap/tests/live.rs`).

### ADR-007 Q15 bounds

Snapshot serving is P3 and global: 4 sessions, 1 per peer, 16 MiB/s. Over a bound the request is answered with an empty message of the right type and the same id, the peer is scored `RateExceeded`, and a peer that is rejected more than 3 times in a row is disconnected (`Handled::RateExceeded`, `Handled::Disconnect`). `Server::admit` opens a session (and says which bound is hit: `Sessions`, `PerPeer`, `Rate`), `Server::release` closes it and charges the bytes that were sent. The rate is a bucket of one second of bytes that refills in proportion to the `now_ms` argument; a response that overdraws it closes the door until time has passed. A message that is not a request is `Handled::Disconnect`. Q7 (8 requests at once per peer, then `DisconnectPeer`) is the transport's: the session reads one request at a time.

Not covered: the 4 sessions are 4 `admit` calls open at once; the in-process loopback is synchronous, so the live path (one task per peer holding a session while it writes) is for the node binding.

## Rust

`crates/xone-snap/src/codec.rs` (messages, caps), `server.rs` (answers, Q15). Tests: `crates/xone-snap/tests/vectors.rs` (10 messages and 9 accounts byte for byte against `oracle/`, non-canonical RLP), `tests/sync.rs` (`server_enforces_the_q15_bounds`, `responses_are_bounded`, hostile input), `conformance/snap/tests/live.rs`.
