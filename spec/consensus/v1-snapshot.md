# XDPoS V1 signer snapshot

## Rule

The V1 snapshot is a Clique-style voting snapshot. It is a value: `apply` returns a new snapshot and reads nothing but its arguments. No lock, no cache and no database are part of the rule (geth keeps them around it; see Not covered).

State: `number`, `hash`, `signers` (a set), `recents` (block number to creator), `votes` (in order) and `tally` (candidate to authorize flag and count).

`apply(headers)` (geth stops at the first failure and returns nothing):

1. No headers: the snapshot itself.
2. The headers are consecutive, and the first is `number + 1` (`InvalidVotingChain`).
3. For each header, in order, with `n = header.number`:
   1. If `n % epoch == 0`, the votes and the tally are cleared.
   2. With `limit = len(signers) / 2 + 1`, if `n >= limit` the entry `n - limit` leaves `recents`.
   3. The creator is recovered from the seal in the last 65 bytes of `extraData` over the seal hash (`Seal`). `recents[n] = creator`.
   4. A vote of this creator for this `coinbase` that is already in `votes` is withdrawn: it leaves `votes` and the tally counts one less (the entry is removed at 1).
   5. The nonce is a vote: all ones is "authorize", all zeros is "drop"; anything else is `InvalidVote`.
   6. The vote is valid if it adds a non-signer or drops a signer. A valid vote joins `votes` and the tally.
   7. If the tally of the coinbase is above `len(signers) / 2`: an "authorize" adds it to `signers`; a "drop" removes it, applies step 2 again with the smaller `limit`, and withdraws every vote that signer cast. Then every vote for that address and its tally entry are removed.
4. `number` is the last header's number and `hash` its hash.

`get_signers` lists `signers` ascending by address bytes. That is the order of the masternodes in a checkpoint header.

`inturn(number, signer)`: with the ascending list, the signer is in turn if its index equals `number % len(signers)`. A signer that is not in the list is never in turn. An empty list is `MissingSigners` (geth divides by zero). This is the snapshot's own rule; V1 header verification does not use it, it uses the turn of the checkpoint's masternode list (see `header-verification-v1.md`, step 11, and `turn.rs`).

`reseed(signers)`: the signer set of a gap block is not voted. At the gap block (`number % epoch == epoch - gap`) geth replaces `signers` with the candidates the 0x88 contract holds in the state after that block, keeps everything else, and stores that snapshot (see `v1-checkpoint-masternodes.md`). A snapshot built by `apply` from an older one, without that step, has the wrong signers from the gap block on. xOneGo `bf73400a` is the same defect: Apothem block 1800 is a checkpoint whose list differs from the previous epoch's, so a snapshot that was not reseeded at block 1350 does not hold the new masternode.

## Source

All at the pin (`XDCIndia/go-ethereum` `6fc8e8c2`).

- `consensus/XDPoS/snapshot.go:46-56` `Snapshot`; `:59-73` `newSnapshot`; `:99-122` `copy`; `:125-128` `validVote`; `:131-142` `cast`; `:145-160` `uncast`.
- `:163-247` `apply`: `:167-174` chain checks (`errInvalidVotingChain`, `xdpos.go:170`); `:180-183` epoch reset; `:184-186` and `:224-226` `recents` limit; `:187-191` creator; `:193-199` withdrawn vote; `:201-209` nonce (`xdpos.go:97-98`, `errInvalidVote` `:118`); `:210-217` cast; `:219-242` threshold; `:244-245` number and hash.
- `:249-263` `GetSigners`; `:266-272` `inturn`.
- `consensus/XDPoS/xdpos.go:269-295` `ecrecover` (the seal is in the last 65 bytes of `extraData`; the digest is `SealHash`, `:201`).
- `consensus/XDPoS/xdpos.go:2156-2211` `UpdateMasternodes`; `:2185-2189` the signer set is replaced; `:2305-2505` `snapshot` (cache, disk, bootstrap, walk-back).

## Networks

Every XDC network with a V1 phase. Apothem has no votes in the sampled windows; the vote paths follow the geth lines above and are tested on synthetic chains (epoch 50, votes, the reset at the checkpoint) against the pinned geth's own snapshots, and on archive windows (below).

## Rust

- `xone_consensus::v1::snapshot::Snapshot` with `new`, `apply(headers, epoch)`, `get_signers`, `inturn`, `reseed` (`crates/xone-consensus/src/v1/snapshot.rs`). Typed errors are `HeaderError` variants.
- Vectors: `snapvotes.jsonl` (24 synthetic chains with votes, 192 snapshots computed by the pinned geth, test `apply_counts_votes_like_geth`) and `conformance/vectors/consensus/v1/snapshot.jsonl`. Each line is a window of consecutive headers with `XDPoS_getSnapshot` of the archive before and after it. The test applies the window in one call and one header at a time and requires the whole snapshot (`number`, `hash`, `signers`, `recents`, `votes`, `tally`) to equal the archive's. Test: `crates/xone-consensus/tests/v1_vectors.rs`, `apply_reproduces_the_archive_snapshot_across_windows`.

## Not covered

- Caches, the disk store, the walk-back from a stored gap snapshot and the bootstrap from the next checkpoint (`xdpos.go:2305-2505`) belong to the runner (XA-05). It must start each epoch from the gap snapshot with `reseed`, never from votes alone (`xdpos.go:2495-2503`, #1505).
- `Prepare` proposals (`xdpos.go:1404-1421`) are a producer rule.

The vote reset at a checkpoint (`votes` and `tally` cleared when `number % epoch == 0`) is exercised by the synthetic chains in `conformance/vectors/consensus/v1/snapvotes.jsonl` (epoch 50), which cross block 50 with votes in flight.
