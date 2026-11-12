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

1. **Head**: the header of the source peer's Status head, fetched by hash and checked to hash to it.
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
     the switch hash and none another (`Witnesses`, `Disputed`). This is geth's trust model (the header chain of
     its peers); acceptable for net2040.
   The pin source (the file, or the peer ids and the hash they served) is in the log line of the pivot and in
   `<datadir>/xone/snap-pivot.txt`.
5. A V1 pivot or a V1 gap pool (the first V2 epoch) is refused (`SnapSyncError::V1`).

## What is downloaded, in order

| step | source | check |
| --- | --- | --- |
| headers from the lowest window floor to the head | eth `GetBlockHeaders`, 192 a request | consecutive, each links to its parent, the last is the peer's head |
| bodies and receipts of `plan_at(P)`'s record window | eth `GetBlockBodies`, `GetReceipts`, 128 a request | `BodyVerifier::verify_body` and `verify_receipts` against the header |
| sign transactions of the window | derived from those bodies and receipts | `signed_hash` of each transaction, sender recovered, receipt status (as the executor records them) |
| candidate pool of each gap block of `plan_at(P)` | snap/1 at the gap block's state root: the 0x88 account by a one-key range proof, its storage ranges with proofs (`account_at`) | the storage root rebuilt from the slots equals the proven account's; the pool is `gap_snapshot::build` on that state |
| the state at `P` | snap/1: accounts, storage, codes, heal | every range proven (`verify_range`), codes by hash, the root of the whole download equals `P`'s state root |
| the rows, by hashed key | the download above | the state root rebuilt from the hashed rows equals `P`'s state root. No preimage is fetched. |

The gap pools are why the window of a serving node is 4,096 blocks (ADR-005 amendment 2026-11-12): the
deepest gap block a plan names is `3E + G - 1` = 3,149 below `P`, `P` is 64 below the server's head.

Request sizes stay inside the server's ADR-007 budgets: one request in flight, 512 KiB asked per range
(`client::Config::default`, under geth's 2 MiB soft limit), and a server that answers over its Q15 bounds
(4 sessions, 1 per peer, 16 MiB/s) answers empty, which the client retries.

## After the install

The node opens the anchored datadir as a minimal node (`--syncmode snap` implies it) and imports `P + 1`
onwards through `Chain::insert`: every block verified and executed against the installed state. Its ethstats
`syncMode` is `snap`.

## Not covered (TODO)

| item | what removes it |
| --- | --- |
| a moving pivot | the client re-pivots on `Unavailable` (`StateStore::set_root`, heal); the node's driver instead fails and the restart loop starts again from an empty datadir. |
| resume of an interrupted download | a persisted `StateStore`; today a stop or crash before the install leaves the datadir empty. |
