# Bootstrap anchor: what a snapshot manifest binds and how it is checked

## Rule

A file hash proves the bytes of a snapshot. It does not prove the chain. A manifest that is only hash-pinned can name the right bytes of the wrong chain, of a forged state, or of a validator set that no longer exists. The anchor binds the snapshot to a chain, a genesis, a checkpoint, a state root and a set of validators, and carries the proof that the validators committed to the checkpoint. `verify_anchor` checks all of it with no I/O and no clock (`xone_sync::bootstrap::anchor`).

### What a hash-pinned download does not prove

| Not proved by the file hash | Bound by |
| --- | --- |
| it is for this chain | `chain_id` against the chain spec |
| it starts from this genesis | `genesis_hash` against the spec's `Genesis.ToBlock().Hash()` |
| the state root belongs to the checkpoint | `state_root` equals the root of the checkpoint header, whose hash is `hash` |
| the checkpoint is on the canonical chain, not a side branch | a QC for it and a 3-chain above it (V2), or seals above it (V1) |
| the signers of that proof are the validators of the epoch | the set is read from the epoch-switch header named in `provenance`, and the QC signatures are checked against it (`v2::cert::verify_qc`) |
| the manifest is not an old one replayed | `created_at` and `max_age`, against the verifier's clock and its own cap |
| the format is one this build reads | `version`, read before any other field |
| the validators were ever the real ones | nothing in the manifest. One pin from outside it is required (see "What is still trusted") |

### Fields

`Anchor` is an RLP list in this order. `version` is read first, so a later format is refused as `UnsupportedFormat` and not as an encoding error of fields this build does not know.

| field | meaning |
| --- | --- |
| `version: u16` | `FORMAT_VERSION` = 1 |
| `chain_id: u64` | `ChainSpec.chain_id` |
| `genesis_hash: B256` | `ChainSpec.genesis_hash` |
| `number: u64`, `hash: B256` | the checkpoint, which is the pivot |
| `state_root: B256` | the post-state root of the checkpoint |
| `created_at: u64`, `max_age: u64` | seconds since the epoch, and how long the creator says it stays usable |
| `provenance { switch_number, switch_hash, validators }` | the epoch-switch header (V2) or epoch checkpoint (V1) that lists the validators, and the list as that header gives it |
| `headers: Vec<Header>` | the proof: one linked run from the provenance header to the end of the proof |

### Verification order

The cheap refusals come first. The first failure is the error.

1. `version` is `FORMAT_VERSION`, else `UnsupportedFormat`.
2. `chain_id`, else `WrongChain`; `genesis_hash`, else `WrongGenesis`.
3. Freshness: a manifest dated after `now` is `FromFuture`; older than `min(max_age, cap)` is `Stale`. The cap is the verifier's (`Context.max_age_cap`), so a manifest cannot extend its own life.
4. A pin: the checkpoint `(number, hash)` or the provenance `(switch_number, switch_hash)` is in `Context.pins`, else `NotPinned`.
5. Binding: the first header is the provenance header (number and hash), the run is contiguous and each parent hash links (`BrokenChain`), the header at `number` has hash `hash` (`CheckpointHash`) and root `state_root` (`RootMismatch`).
6. The proof (V2 or V1, below).

### V2 proof (checkpoint above the V2 switch block)

- The first header is an epoch switch (`epoch_switch::of_header`, else `NotEpochSwitch`) whose `validators` list equals `provenance.validators` and is not empty (`ProvenanceValidators`). It must be at the start of the proof: no later header up to the end of the proof is an epoch switch (`EpochSwitchInProof`), because the certificates of one epoch are signed by one set. So the checkpoint is at least four blocks before the next epoch switch.
- The proof ends three blocks above the checkpoint (`TooShort`). For each header above the checkpoint, its QC (in `extraData`) certifies the previous header: same hash, round and number (`QcBlockMismatch`); its gap number is the one of the epoch of the switch header, `gap_number(switch, E, G)` (`GapNumber`, geth `utils.go:417-433`); its signatures verify against the switch header's list with the `certThreshold` of the config at the QC's round: a signature that does not recover, is high-S, or is by a non-member is `ForgedCertificate`; fewer distinct signers than the threshold is `UnderThreshold`; a repeated signature counts once.
- The 3-chain rule (`v2::commit::commit_blocks`, geth `utils.go:530-598`) over the proof commits exactly the checkpoint (`NotCommitted`): three consecutive rounds, the QC of the third in the last header.
- The first V2 epoch has no V2 switch header: the V2 switch block is a V1 header and the block above it is the first epoch switch, with the list in `validators` (Apothem 56,828,701: 13 masternodes, round 1, a QC with no signatures for round 0). A proof that starts on the V2 switch block is refused (`ProvenanceValidators`); start at the block above it. Test `real_anchor_in_the_first_v2_epoch_starts_above_the_v2_switch_block`.

### V1 proof (checkpoint at or below the V2 switch block)

V1 has no certificate and no finality. The rule here is the anchor's own, not geth's.

- The first header is an epoch checkpoint (`number % E == 0`) whose `extraData` lists the masternodes of the epoch above it (`masternodes_of`, `GetMasternodesFromCheckpointHeader`, `xdpos.go:2122-2127`), equal to `provenance.validators`.
- The checkpoint of the anchor is above that header and the run stays inside its epoch (`V1Position`). A block on the epoch checkpoint is sealed by the previous list, so it is not an anchor.
- Every header above the epoch checkpoint has a seal that recovers (`V1Seal`) from a signer in the list (`V1Unauthorised`). The distinct signers from the anchor block to the end of the run are at least `ceil(2n/3)` of the `n` listed (`V1Depth`), the V2 two thirds in integers.

### Freshness policy

`now - created_at <= min(max_age, max_age_cap)`. The reason is the validators: an old manifest names a set that may have rotated, and a stale set is the usual way a long-range history is made to look signed. The cap is a deployment parameter of the verifier; this crate sets no value (the tests use 7 days and a manifest `max_age` of one day). The owner decides the cap, and it can be per network.

### What is still trusted

The epoch-switch header lists its masternodes, and a QC is signed by those masternodes. A chain that someone builds from its own keys, with its own set in the first header, passes every check above (test `a_manifest_signed_by_its_own_keys_is_not_pinned`). No header proves that its list was elected: that is checked by executing the chain from a point the verifier trusts. So one hash must come from outside the manifest: a checkpoint list that ships with a release, or an operator flag. The verifier takes `Context.pins`. Without a pin that matches the checkpoint or the provenance header the answer is `NotPinned`.

The anchor reduces the trust from "a file I downloaded" to "one 32-byte hash I was given", and it makes that hash bind the chain, the state root and a committed block, not only bytes.

## What geth-XDC does

- `TrustedSyncCheckpoints` (`params/config.go:2149-2162`, `TrustedSyncCheckpoint` `:2279-2285`): `Number`, `Hash`, `Root`, and for V2 anchors `Masternodes` and `Penalties`. Mainnet `:119-` has 19 entries (7 with a masternode list: 80,370,000, 80,999,550, 97,999,650, 98,799,750, 98,801,550, 101,999,250, 102,513,150), Apothem `:1086-` has 16 (9 with a list, the last at 82,293,750). V2 entries are gap blocks (`Number % 900 == 450`, `:114`, `:1082-1085`). There is no certificate and no seal chain: the binary is the whole proof, and the masternode list is typed into it. The field is deprecated: "Do NOT add new entries" (`:2149-2162`), superseded by a QC-verified pivot.
- `AutoPinPivotFromCheckpoints` takes the highest entry with a non-zero hash, a non-zero root and a non-empty list (`eth/ethconfig/auto_pivot_xdc.go:72`, `:97`).
- The QC-verified pivot (`engine_v2/snap_pivot.go:27-247`): the candidate is at least 12 blocks below the peer's head, and its three children carry QCs that verify (`:218-247`). The epoch-switch header whose list they are checked against is fetched from the peer (`:258`), and the code says so: "the serving peer is the trust root ... the QC signature verification that follows provides the cryptographic check" (`:252-254`). A peer that serves its own switch header with its own list passes. This is the gap the pin closes.
- After the pivot, `inCheckpointCatchup` skips QC verification and the 3-chain commit for two epochs (`engine_v2/utils.go:281-311`). xOne verifies every block and keeps the window of `spec/retention.md`.

## Rust

`xone_sync::bootstrap::{Anchor, Provenance, Context, FORMAT_VERSION, AnchorError, verify_anchor}`; the proof reuses `xone_consensus::v2::{cert::verify_qc, commit::commit_blocks, epoch_switch::of_header, gap_snapshot::gap_number}` and `xone_consensus::v1::checkpoint::{addresses, masternodes_of}`.

## Vectors

Real, from the Apothem archive (`xdc02:21105`, read-only, 2026-11-11, `tests/bootstrap/`): the epoch switch 84,954,819 and 15 blocks above it (95 masternodes, 64 signatures a QC; the checkpoint 84,954,831 and the epoch switch itself as the checkpoint), the answer of `XDPoS_getMasternodesByNumber` for the same set, the V2 switch block 56,828,700 and 15 blocks above it, the V1 checkpoint 50,000,400 and 17 blocks above it (10 masternodes, 7 sealers needed). Every header is checked against the hash the archive reports. Synthetic, signed with test keys: wrong chain, wrong genesis, wrong root, stale, from the future, future format, forged, under-threshold, duplicate signer, high-S twin, a signature that recovers nothing, no pin, a foreign chain under an honest pin, a skipped round, an epoch switch inside the proof, a wrong gap number, a QC for another block, a broken link.

## Not covered

The snap/1 wire; the export and import of the snapshot files (XB-18), which must read the `first_block` of each class against `Retention::floor`; the proof that the gap snapshots shipped with the snapshot (`spec/retention.md` rows 3, 7, 11) are the pool of the 0x88 state at their gap blocks (they are state-derived, and the manifest of the snapshot has to carry a proof against the state root of the gap block, which the headers alone cannot give); the first post-pivot replay (XA-05); the value of the freshness cap and the source of the pins (owner decisions).
