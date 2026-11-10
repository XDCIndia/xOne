---
name: xone-rs-conformance-loop
description: The core develop-and-verify loop of the xOne Rust rebuild. Pick the next specification item, write its golden-vector test from geth-XDC, implement it, run it, diff against the geth archive, fix, and gate. Use when the task says implement, port, add or fix a consensus, EVM, state, chainspec, sync or storage rule in XDCIndia/xOne; when a milestone work item is assigned; when a divergence record must be turned into a fix; or when someone says "conformance loop", "next spec item", "golden vector" or "make it bit-exact".
---

# xone-rs-conformance-loop

geth-XDC (`XDCIndia/go-ethereum`, pinned commit in `conformance/PIN`) is the reference specification and the xdc02 geth archive is the judge of canonical history. One oracle can share its own mistakes, and canonical history never tests rejection. So the loop also uses independent spec review, negative cases and a property tier (below). It never ends on an opinion; it ends on a differential result plus rejection evidence.

## Operating rules

1. One work item per loop run, one PR per work item.
2. Test first. The golden-vector test exists and fails before any implementation is written.
3. Expected values come only from the exporter (`conformance/exporter`) reading the pinned source, or from a named external fixture set. Never type or edit an expected value by hand. **Never rewrite an expected output to make a test pass.** If an expected value looks wrong, write a divergence record and escalate; a change goes through the exporter or fixture re-pin, with a reviewer who did not write the code.
4. No bypass, skip list, tolerated mismatch or validation switch. A wrong root is a stop.
5. In `xone-chainspec`, `xone-consensus`, `xone-evm` and `xone-state`: no `unwrap`, `expect`, `unwrap_or_default`, dropped `Result`, numeric `as` cast, `std::env::var`, or global mutable state. "Missing" is a typed error, never an empty list or a zero.
6. Fork, SDK, selected crates, small maintained fork, or Go reuse is a Gate-A evidence decision recorded as an ADR. Until it is recorded, do not fork revm or reth. If an item needs something the ADR does not allow, stop and escalate.
7. Follower only. This skill never starts a producer; see `xone-rs-mint-gate`.

## Vector tiers and provenance

Every vector and every spec page names its **provenance**: geth `file:line` at the pin, or an Ethereum execution-specs fixture id and version (the old execution-spec-tests repository is archived and points fixture consumers to execution-specs; check the current home), or a synthetic generator with its seed and its rule source. A vector with no provenance is rejected.

| Tier | Content | Source |
|---|---|---|
| Positive | Canonical blocks, epochs, forks, rewards | Exporter over the archive |
| Negative | Invalid blocks and inputs that must be rejected with a typed reason: bad `Validators`, bad `Penalties`, bad QC or signature, body not matching header, wrong roots, wrong fork-gated field | Applicable execution-specs invalid-block fixtures; XDC synthetic cases built from the spec |
| Property / synthetic | Unwind then re-execute, encode/decode round trips, wide-balance boundaries, journal revert, generated sequences with a saved seed | proptest and generators; seeds saved |
| Cross-client | Same input to a second client where one exists | Hive-style or direct comparison |

Every rule needs at least one negative vector. A rule with only positive vectors is not done.

## Inputs

- The milestone issue and the work item (rule name, fork range, network).
- The pinned geth commit and the path to its checkout.
- Archive RPC endpoint (xdc02), read-only.

## Steps

1. **Pick.** Take the next unchecked item of the milestone. If none is listed, take the first failing vector class.
2. **Specify.** Write or update `spec/<rule>.md`: inputs, outputs, the fork that changes the rule, and geth `file:line` at the pin. Use `xone-rs-consensus-parity` for the checklist entry.
3. **Review the page.** The planning tier compares the page with the geth source before code is written.
4. **Vector.** Run the exporter for the rule's block or epoch range. Add positive, negative and property vectors under `conformance/vectors/<rule>/`, each with provenance. Add a mutant switch under `conformance/mutants/` that re-introduces the wrong behaviour.
5. **Red.** Run the table test. It must fail, including the negative vectors (the unfixed code must accept them or the test is wrong). Run the mutant. The harness must report it.
6. **Implement.** Smallest change in the owning crate. Types from `xone-primitives` (`BlockNumber`, `Round`, `Epoch`, `GapNumber`, `WideBalance`), never bare integers.
7. **Green, local.** `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo nextest run -p <crate>`, miri for primitives and consensus crates, and the sidecar function check.
8. **Differential.** Run `xone-rs-diff-exec` in sequential mode on the item's range, then on the gate range. Oracle-prestate runs are diagnostic only.
9. **Fix or escalate.** On a mismatch: write the divergence record, find the root cause, add a regression vector that fails, go to step 6.
10. **Independent check.** A checker who did not write the code re-runs steps 7 and 8 on a clean checkout.
11. **Gate.** When all items are done, run `xone-rs-fork-boundary-gate` for the gate in scope.

## Retry and escalation

- The same item fails verification **three times**: stop and escalate to the planning tier with the three divergence records. Do not try a fourth variant.
- A P0 (wrong state root, receipts root, gas or bloom on an accepted block): stop the milestone, not only the item.
- A needed fork of revm or reth, a consensus RLP or wire change, or a geth behaviour that looks wrong: stop and ask the owner. geth is still the reference until the owner says otherwise.

## Gates

- Red before green is shown in the PR (failing test output, then passing).
- 0 mismatches on the item range and on the gate range (sequential), with block range and commit recorded.
- Every negative vector is rejected with the expected typed reason; every positive vector is accepted.
- No expected output was edited to pass (the PR diff of `conformance/vectors/` comes from the exporter or a re-pin).
- The mutant is detected.
- The independent checker agrees.

## Done

The PR links the spec page, lists the vectors added, shows the differential result (range, mismatches = 0, commit, host) and the mutant result, and the milestone checklist item is ticked. Commit identity and trailers follow `xone-rs-release`.
