# V2 certificates: QC, TC and the blank bootstrap TC

## Rule

**Which signatures count** (geth `recoverUniqueSigners` with the allowed set). Every signature of the certificate is looked at, in order, and the first failure refuses the whole certificate:

1. A signature that does not recover a public key (wrong length, `v` out of range, no curve point) is refused (`SignatureUnrecoverable`).
2. A signature whose `s` is above half the curve order is refused (`SignatureHighS`). The malleated twin `(r, N - s)` recovers the same address, so it would be a second vote of one key.
3. A signature that recovers an address outside the masternode list of the epoch is refused, and the certificate with it (`SignerNotMasternode`). A non-member does not count and does not pad: it is not skipped.
4. A signature byte-identical to an earlier one is skipped. Two signatures that recover the same address count once: one key can make any number of distinct valid signatures with random nonces (Hacken F-2026-19525).

The count is the number of distinct recovered addresses.

**Threshold.** The certificate passes when `float64(count) >= float64(len(masternodes)) * certificateThreshold`, a float comparison with no rounding step. `certificateThreshold` is the value of the V2 config at the round of the certificate (QC: the round of the certified block; TC: its own round). With 0.667, 3 masternodes need 3 signers (2.001 > 2) and 108 need 73 (72.036). `min_signers` is the smallest count that passes, found by counting, so it is exact for every width.

**QC** (`verifyQC`). The masternodes are those of the epoch that holds the certified block. A QC of round 0 (the certified block is the V2 switch block) skips the threshold but not the checks above. The gap number and the block info (hash, number, round; the switch block has round 0) are checked by the header verifier, in this order after the signatures.

**TC** (`verifyTC`). The gap snapshot at `tc.gap_number` must exist and have candidates; this is only a sanity check, the signers are checked against the masternodes of the epoch that holds the TC's round. There is no round-0 exemption: a TC without signatures fails the threshold.

**The blank bootstrap TC.** A node that has seen no timeout holds a TC of round 0 without signatures (`isBlankTC`; the gap number is not looked at). geth accepts it in exactly one place, the TC of a SyncInfo (`VerifySyncInfoMessage`), after the QC of that SyncInfo has passed. Anywhere else (`verifyTC`) it fails. Every other TC, malformed or not, is verified in full.

**Decoding.** A certificate is one RLP value: trailing bytes, a missing or an extra element, a non-canonical integer, a signature that is a list, are all refused (`rlp.DecodeBytes`). `alloy_rlp::decode_exact` is the same rule, and the vectors show it.

## Source

- `engine_v2/utils.go:131-250` `recoverUniqueSigners`; `:86` `hasLowS`; `:125` `errSignerNotMasternode`; `:210` member check; `:221` byte-duplicate skip.
- `engine_v2/utils.go:315` `verifyQC`; `:363-373` the threshold gate (`qcRound > 0 && ...`); `:417` gap number; `:619` `VerifyBlockInfo`.
- `engine_v2/timeout.go:373` `verifyTC`; `:383-394` empty snapshot; `:334` `getTCEpochInfo`; `:429-436` the gate.
- `engine_v2/utils.go:672` `VerifySyncInfoMessage`; `:714` the blank-TC branch; `:739` `isBlankTC`; `blank_tc_test.go` for the accepted forms.
- `core/types/consensus_v2.go:227-247` `VoteForSign`, `TimeoutForSign`: the digest is `keccak256(rlp([info, gap]))` and `keccak256(rlp([round, gap]))`.
- F-2026-19525: `pool_signer_dedup_test.go`, `threshold_signers_1552_test.go`.

## Rust

`xone_consensus::v2::cert::{qc_digest, tc_digest, unique_signers, min_signers, verify_qc, verify_tc, TcContext, is_blank_tc, verify_sync_info_tc, decode_qc, decode_tc}`; `HeaderVerifier` calls `verify_qc` and keeps the gap number and block info checks.

## Vectors

`conformance/vectors/v2/cert.json` (oracle `crates/xone-consensus/oracle/v2`, command `cert`): 220 cases whose accept or reject is the answer of the pinned geth `verifyQC`, `verifyTC` and `VerifySyncInfoMessage` run on a seeded engine: exact-minimum and one-below at 24 (n, threshold) pairs chosen where `float64(n) * threshold` falls near an integer, duplicates, one signer with many nonces (Hacken), a non-member as an extra and as a replacement, a high-S twin, garbage and short signatures, round-0 QCs, a threshold that changes with the round, empty snapshot, blank TC direct and in a SyncInfo, and 22 decoding cases. `tests/v2_header.rs` runs `verify_qc` on 277 real QCs of the header corpus and five mutations of every fifth.

## Not covered

- The walk back to an earlier epoch when the TC's round is before the epoch of the current head (`getTCEpochInfo`, `timeout.go:334-370`) needs a lookup of epochs by number; the driver (XB-15) supplies the masternodes of that epoch to `TcContext`.
- `VerifySyncInfoMessage` returns early when the SyncInfo is not newer than the local state (`utils.go:683-693`); that is local state of the voting engine (XB-15).
- The signature-count cap of peer messages (`maxPeerSigCap`, `utils.go:696`) belongs to message intake.
