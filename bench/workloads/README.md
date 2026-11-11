# Workload specs

One YAML file per workload in `docs/perf.md` section 5. Each carries the pinned workload fields of the contract: network, chain id, fork-table pin, block ranges, load model and levels, `input_hash`, generator, node mode, durability, state, repeats, tolerance, metrics.

- `input_hash` is empty until the inputs are generated. It is sha256 over the ordered input bytes (blocks as stored in the archive, or generated transactions, in the order replayed). Never edit it by hand; the generator writes it.
- Ranges marked `status: candidate` are windows to be confirmed against the Apothem archive before generation. Ranges marked `nominal: true` (epoch switches) must be resolved against the archive; the resolved heights and hashes go in the run record.
- Boundaries come from `crates/xone-chainspec/generated/forks.json`: V2 switch 56,828,700; EIP-1559 71,550,000; Cancun 71,551,800; Prague 84,333,000; proportional reward 84,733,000.

A run record (template in `bench/README.md`) must reference a workload by `workload.id`, the spec file path, the git SHA of the spec file, and the filled `input_hash`. A record whose `input_hash` is empty, or whose spec SHA differs from the repository at the recorded commit, is invalid.
