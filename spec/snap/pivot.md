# Snap sync in the node: the pivot rule

Code: `xone_node::services::snapsync` (the driver), `xone_snap::client` (state download, `account_at`),
`xone_sync::bootstrap::{build_anchor, verify_anchor, plan_at}`. Flags: `--syncmode snap` (`fast` is the
same, as in geth-XDC), `--snap.pins <file>`. Install: [snap-install](../bootstrap/snap-install.md).
Preimages: [preimages](preimages.md).

## When it runs

`--syncmode snap` on an **empty** datadir (no `head.json`, no stored chain). A datadir that holds a chain
continues it: a full datadir follows in full, an anchored one (from a snap sync or `snapshot import`) as a
minimal node. The node does not serve RPC or the board during the download; it starts them once the pivot
is installed.

## Peers

State comes from a peer that shares **snap/1**: a geth-XDC node or an xOne node. snap/1 carries hashed keys
and no preimages, and the xOne state is keyed by hash too (ADR-005 amendment 2026-11-12: `HashedAccounts`,
`HashedStorages`), so the download fills the datadir as it is. After 120 s without a snap/1 peer the start
fails with `SnapSyncError::NoSnap` (never a fallback to full sync). Headers, bodies and receipts come from the
same peer; every peer may be a witness for the pin.

## The pivot

1. **Head**: the header of the source peer's Status head, fetched by hash and checked to hash to it, then the
   headers after it up to the peer's tip, each linked to its parent (the Status head is as old as the
   connection; after a move the pivot must come from the tip of now).
2. **Candidate**: `head - 64`. geth's rule is the same distance (`fsMinFullBlocks`,
   `eth/downloader/downloader.go`; geth-XDC keeps 64 for XDPoS, `eth/downloader/a89_dynamic_anchor_test.go:102`
   "returns fsMinFullBlocks (64), NOT 2×Epoch"). The distance keeps the pivot committed (V2 commits a block
   three blocks later) and inside the server's state window.
3. **Anchor**: from the candidate down by at most 64 blocks, the first block `P` for which
   `build_anchor` finds a proof in the downloaded headers (the nearest epoch switch at or below `P`, no switch
   in `P+1..=P+3`, the head at least `P+3`) and `verify_anchor` accepts it: the switch header's masternode
   list, the QCs of `P+1..=P+3` signed by at least the threshold of that list, and the 3-chain that commits `P`.
   This is the same check as `snapshot import` (`spec/bootstrap-anchor.md`), with the anchor built from
   headers the peers served instead of a manifest file.
4. **Pin**: `verify_anchor` needs the switch header (or `P`) pinned from outside the proof.
   - `--snap.pins <file>` (one `<number> <0x hash>` per line, the `snapshot import --pins` form): the
     operator's pin. **Recommended for anything beyond a test network.**
   - Without it: every connected peer is asked for the header at the switch number; at least two must serve
     the switch hash, and more peers must serve it than serve any other hash (`Witnesses`, `Disputed`). A peer on
     another chain (a stale release) is outvoted and logged, not fatal. This is geth's trust model (the header
     chain of its peers); no `--snap.pins` file is needed, and a fresh pivot after a move gets its own switch
     header the same way. Acceptable for net2040 and Apothem.
   The pin source (the file, or the peer ids and the hash they served) is in the log line of the pivot and in
   `<datadir>/xone/snap-pivot.txt`.
5. A V1 pivot or a V1 gap pool (the first V2 epoch) is refused (`SnapSyncError::V1`).
6. **Total difficulty** (#119): `td(P)` is the source's Status td at its Status head `H`, less the difficulty
   of the linked headers `P + 1 ..= H` (plus `H + 1 ..= P` when the Status is older than `P`). geth-XDC puts
   the stored TD of its head in Status (`xdcHandshakeTD`, `eth/protocols/eth/handshake.go:87-104`), counted
   from genesis, and a V2 block adds 1 (`engine_v2/engine.go:859-861`). The install writes `td(P)` into the
   pivot's checkpoint and the node adds each imported block's difficulty, so its Status td equals a full
   node's at the same head. The value is the source's claim (geth trusts a Status td the same way); a
   missing header between `P` and `H`, or a saturated td, leaves it 0 and the node reports the window's sum.

## What is downloaded, in order

| step | source | check |
| --- | --- | --- |
| headers from the lowest window floor to the head | eth `GetBlockHeaders`, 192 a request | consecutive, each links to its parent, the last is the peer's head |
| bodies and receipts of `plan_at(P)`'s record window | eth `GetBlockBodies`, `GetReceipts`, 128 a request | `BodyVerifier::verify_body` and `verify_receipts` against the header |
| sign transactions of the window | derived from those bodies and receipts | `signed_hash` of each transaction, sender recovered, receipt status (as the executor records them) |
| candidate pool of each gap block of `plan_at(P)` | in this order: (a) an xOne peer (snap/1 and xpre/1) at the gap block's state root: the 0x88 account by a one-key range proof, its storage ranges with proofs (`account_at`); (b) `--snap.gap-rpc <url>`: `eth_getProof(0x88, slots, G)` for exactly the slots `gap_snapshot::build` reads (candidates length and entries, then each cap; geth `engine_v2/engine.go:1349-1360`); (c) neither: `SnapSyncError::GapSource`, which names both. Never an empty pool. | (a) the storage root rebuilt from the slots equals the proven account's; (b) the account proof against the state root of the header of `G` (already linked) and each slot proof against the proven storage root (`xone_snap::rpcproof`), so the RPC is untrusted; the pool is `gap_snapshot::build` on that state |
| the state at `P` | snap/1: accounts, storage, codes, heal | every range proven (`verify_range`), codes by hash, the root of the whole download equals `P`'s state root |
| the rows, by hashed key | the download above | the state root rebuilt from the hashed rows equals `P`'s state root. No preimage is fetched. |

The gap pools are why the window of a serving node is 4,096 blocks (ADR-005 amendment 2026-11-12): the
deepest gap block a plan names is `3E + G - 1` = 3,149 below `P`, `P` is 64 below the server's head.

A geth-XDC peer serves snap/1 only for its recent snapshot layers, so the state at the pivot (64 blocks below its head) comes from it but the 0x88 state of the gap blocks does not: the V2 tiered reward at an epoch switch reads the standby pool of the epoch two switches back (`eth/hooks/engine_v2_hooks.go:878-941`, `GetStandbynodes`, `engine_v2/engine.go:3128-3147`), whose gap block is below the pivot. That is why step (a) to (c) above exists.

Request sizes stay inside the server's ADR-007 budgets: one request in flight, 512 KiB asked per range
(`client::Config::default`, under geth's 2 MiB soft limit), and a server that answers over its Q15 bounds
(4 sessions, 1 per peer, 16 MiB/s) answers empty, which the client retries.

## A moving pivot (re-pivot)

A peer serves state only for its recent roots (a geth path-scheme node: the last ~128 blocks, about 4 minutes
on a 2 s chain), so a state download that outlasts them is answered "no data" by every peer
(`SyncError::Unavailable`, after `max_failures` empty answers). The driver does what geth's snap syncer does:

1. pick a fresh pivot by the rules above (new tip, `head - 64`, anchor verified, switch header pinned again);
   the window of headers, bodies, receipts and the gap pools are fetched again for it; the fault counts of
   the peers are cleared;
2. `StateStore::set_root(new root)`: the downloaded accounts, storage and codes stay and the cursors go on;
3. `client::sync` runs again: it reconciles the downloaded accounts with the new root (`GetTrieNodes`), downloads
   the rest at the new root, heals, and checks the root. A later `Unavailable` repeats from 1.

Nothing is installed until the root of the whole download equals the state root of the last pivot, so the
anchor, the window and the state of the installed datadir are all of one pivot. At most 64 moves (the log
names each: `pivot moved to root ... (move n); k accounts kept`), then the sync fails with the last error.

## After the install

The node opens the anchored datadir as a minimal node (`--syncmode snap` implies it) and imports `P + 1`
onwards through `Chain::insert`: every block verified and executed against the installed state. Its ethstats
`syncMode` is `snap`.

## Not covered (TODO)

| item | what removes it |
| --- | --- |
| resume of an interrupted download | a persisted `StateStore`; today a stop or crash before the install leaves the datadir empty. |
| memory | the `StateStore` is in memory (also across moves, about 150 bytes a slot): the heal builds nodes only for the storage tries that differ from the root, one at a time, and the install moves the rows out of the store; a network whose state outgrows RAM still needs the persisted store (and a crash restarts from empty). |
