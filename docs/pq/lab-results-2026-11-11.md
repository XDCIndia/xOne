# PQ cost lab results, 2026-11-11

Work item: [#18](https://github.com/XDCIndia/xOne/issues/18) (XA-13), post-quantum track A. Research only. No activation, no security claim implied.

**This is a single-host measurement taken under the contract in [`docs/perf.md`](../perf.md). It is not a target and not a requirement.** It was taken on a shared host that was busy with other work (section 2), so it does not meet the contract's "no other load" rule. The numbers are valid for this binary, this host and this day only. Use them to compare schemes on one loop, not to size a node.

All libraries are **lab input until reviewed** (see [`implementations.md`](implementations.md)). Timing a library says nothing about its security. "Quantum-resistant under stated assumptions" is the only claim NIST makes for these algorithms (see [`standards.md`](standards.md)).

## 1. What was measured

One timing loop (`labs/pq/src/main.rs`, crate `pq-lab`) over every scheme. Signed message: a 32-byte digest, the shape the chain signs today. Operations:

| Operation | Meaning |
|---|---|
| `keygen` | Deterministic key generation from a fixed seed (no OS RNG). ECDSA: scalar to public key |
| `sign` | One signature over the 32-byte message. `ml-dsa`: the deterministic FIPS 204 variant, empty context. `fips204` and `fips205`: hedged (OS randomness per call), context `xone-pq-lab`. ECDSA: RFC 6979, recoverable `r \|\| s \|\| v` |
| `verify_valid` | Verify a valid signature from its wire bytes (the decode of the signature is inside the timed call; the public key is already decoded) |
| `verify_invalid_msg` | Verify a well-formed signature made over another message. Full verification, then rejection. This is the expensive reject an attacker can send |
| `verify_invalid_flip` | Verify the valid signature with one bit flipped in the middle of its bytes |
| `recover` | ECDSA only: recover the public key from the signature (the chain's `ecrecover` path; no address hashing) |

Every call asserts its expected outcome (valid verifies, invalid rejects), so an "invalid" row cannot silently time a valid signature. The lab was also run through its unit test (round trip and rejection for ECDSA, `ml-dsa` ML-DSA-44 and `fips204` ML-DSA-44).

Libraries (exact pins, `Cargo.lock`): `k256 0.13.4` (workspace pin, feature `ecdsa` only, so no precomputed tables), `ml-dsa 0.1.2` (features `alloc`, `getrandom`), `fips204 0.5.0` (cross-check of ML-DSA), `fips205 0.5.0` (SLH-DSA-SHAKE-128s, -128f, -192f). Reasons for the choice are in `implementations.md` section 2.

Sampling. 5 warm-up calls, then calls until 1,000 samples, or until 30 s have passed and at least 30 samples exist. Fast operations reach 1,000. Slow ones stop earlier: where `n` is below 100 in the table, p99 is the rank-`n` value, that is the maximum, and carries little information. Percentiles use nearest rank with integer arithmetic, the rule of `bench/collect/summarise.py`. Time is `Instant` around one call (about 20-30 ns overhead, negligible against 50 us and more). The raw per-sample values are in `labs/pq/results/2026-11-11/run*.csv.gz`; the Rust percentiles were cross-checked against `bench/collect/summarise.py` (section 6).

Three runs, one after another, each a fresh process, same binary and pinned CPU. The first run was after a build, so its page cache for the binary was cold; the three are not separated otherwise. The table gives the median of the three runs' percentiles and the spread (max minus min as a percentage of the median), as `docs/perf.md` section 4 asks. The contract tolerance is 10 percent for p50 and 20 percent for p99. Rows outside it are listed in section 4.

## 2. Environment record

Raw record from `bench/collect/env.sh`: `labs/pq/results/2026-11-11/env.json` (captured while run 1 was running, with `--pid`). `env.sh` worked on this host. Its `cpupower` call failed (not installed) and its `boost` path does not exist on this kernel; both are in the JSON as written.

| Field (docs/perf.md section 2) | Value |
|---|---|
| Date (UTC) | 2026-11-11, runs started 03:16 UTC |
| Hardware tier label | none assigned by the owner. Not a baseline host. "Shared development host" |
| CPU | 13th Gen Intel Core i5-13500, 6 performance cores with SMT (CPUs 0-11, max 4.8 GHz) and 8 efficiency cores (CPUs 12-19, max 3.5 GHz). 20 logical CPUs. ISA flags (including `avx2` and `sha_ni`, no `avx512`) are in the `lscpu` field of the JSON. The libraries used here have no AVX2 path: portable code only |
| Process placement | `taskset -c 8` (a performance-core thread; SMT sibling CPU 9 not isolated) and `nice -n -5`. One NUMA node |
| Frequency, SMT, boost | governor `powersave` on all 20 CPUs, SMT on, `no_turbo = 0` (boost allowed). Frequency during the runs was not sampled |
| Memory and swap | 64 GB RAM, swap on and almost full (about 31 of 33.5 GB in use by other processes). The lab's own RSS is about 3.6 MB |
| Storage | not used by the lab. Datadir argument for `env.sh`: `/mnt/data/xa13-build` on `/dev/nvme2n1p1`, ext4, `rw,relatime` |
| OS | Linux 6.8.0-101-generic, cgroup v2, `memory.max` and `cpu.max` both "max" (no limit), THP `madvise` |
| Build | `cargo build --release -p pq-lab`, profile `release` (opt-level 3, no LTO, workspace default), `RUSTFLAGS` empty, no `target-cpu`. Toolchain `rustc 1.94.0 (4a4ef493e 2026-03-02)`. Binary SHA-256 `b8a1cca920a34972e78af08740f00f454823a74542015bf3f1a6d3b79097d7bd`. `pq-lab --version`: `pq-lab 0.1.0` |
| Source | branch `feat/xa-13-pq-lab`, base commit `3998012a1a0b30d9ac59dbcd4bb797f48fec6054`, working tree uncommitted. `labs/pq/src/main.rs` SHA-256 `5cd6a150aca96d0d8082fa7a512c5e93b2f47ebcfab2707183d9d291f1359325`; `Cargo.lock` SHA-256 `6eb312a5f42496d0de5e7f0d94101e9a6d2b6986e213fd789ae5a3fc2c3e828a` |
| Chain, node mode, durability | not applicable: there is no chain, no node and no database in this lab |
| Topology | no network, no validator key, no mining, no signing key of any real account. Keys are fixed test seeds |
| Workload | id `pq-lab-sign-verify`; spec: the table in section 1; input: 32-byte messages `0x11` repeated and `0x22` repeated; seeds `0x07` repeated (ECDSA and ML-DSA) and `1`, `2`, `3` repeated (SLH-DSA) |
| State | warm (5 warm-up calls); fresh process per run |
| Load generator | none: closed-loop, one call at a time, by design for a CPU microbenchmark (`docs/perf.md` rule 1 allows this) |

Load on the host. `/proc/loadavg` before and after each run (1, 5, 15 minute averages, running/total threads):

| Run | Start (UTC) | Load average at start (1, 5, 15 min) | End (UTC) | Load average at end |
| --- | --- | --- | --- | --- |
| 1 | 2026-11-11T03:16:41Z | 14.11 27.30 36.62 | 2026-11-11T03:22:00Z | 33.70 35.92 38.21 |
| 2 | 2026-11-11T03:22:00Z | 33.70 35.92 38.21 | 2026-11-11T03:26:03Z | 15.24 24.19 32.80 |
| 3 | 2026-11-11T03:26:03Z | 15.24 24.19 32.80 | 2026-11-11T03:30:14Z | 20.62 22.24 29.97 |

All 20 logical CPUs were between 41 and 58 percent busy in a 3 s sample before the runs; the 1-minute load average was between 14 and 72 during the session. Other processes on the host (a Rust build, database, container and node services) shared the CPUs. **Contract deviations:** shared host with load; no isolated core; SMT sibling not idled; governor `powersave`; swap pressure; hybrid CPU; no `target-cpu=native`; three runs rather than a soak; no cold/warm separation beyond the fresh process.

## 3. Sizes

Sizes are read from the library objects and checked against the FIPS tables in `standards.md` (they match). Bytes.

| Item | ECDSA | ML-DSA-44 | ML-DSA-65 | ML-DSA-87 | SLH-128s | SLH-128f | SLH-192f |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Public key | 33 | 1,312 | 1,952 | 2,592 | 32 | 32 | 48 |
| Signature | 65 | 2,420 | 3,309 | 4,627 | 7,856 | 17,088 | 35,664 |
| Signature, ratio to ECDSA 65 B | 1.0x | 37.2x | 50.9x | 71.2x | 120.9x | 262.9x | 548.7x |

ECDSA: 33-byte compressed public key; 65-byte recoverable signature (`r`, `s`, `v`), no public key needed on the wire because the signer is recovered.

## 4. Latency results (microseconds)

| Library | Scheme | Operation | n per run | p50 us (median of runs) | p50 spread | p99 us (median of runs) | p99 spread | min us (median of runs) | p50 per run | p99 per run |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| k256 | ECDSA-secp256k1 | keygen | 1000/1000/1000 | 44.4 | 13% | 102.4 | 14% | 39.4 | 44.4/42.9/48.5 | 102.8/88.7/102.4 |
| k256 | ECDSA-secp256k1 | sign | 1000/1000/1000 | 61.7 | 21% | 117.4 | 22% | 50.3 | 50.9/63.6/61.7 | 94.3/120.6/117.4 |
| k256 | ECDSA-secp256k1 | verify_valid | 1000/1000/1000 | 68.7 | 14% | 162.6 | 10% | 57.3 | 68.7/72.8/63.2 | 163.0/162.6/147.0 |
| k256 | ECDSA-secp256k1 | verify_invalid_msg | 1000/1000/1000 | 70.7 | 30% | 154.1 | 14% | 58.6 | 68.9/70.7/90.0 | 154.1/146.2/167.7 |
| k256 | ECDSA-secp256k1 | verify_invalid_flip | 1000/1000/1000 | 69.6 | 14% | 150.7 | 10% | 60.2 | 74.4/69.6/65.0 | 161.3/150.7/146.7 |
| k256 | ECDSA-secp256k1 | recover | 1000/1000/1000 | 153.3 | 28% | 341.3 | 2% | 131.3 | 194.6/151.1/153.3 | 346.0/338.1/341.3 |
| ml-dsa | ML-DSA-44 | keygen | 1000/1000/1000 | 150.9 | 8% | 318.8 | 4% | 125.9 | 159.3/150.9/147.4 | 312.2/318.8/325.8 |
| ml-dsa | ML-DSA-44 | sign | 1000/1000/1000 | 2,172.8 | 36% | 4,137.0 | 5% | 1,657.7 | 2,172.8/2,101.1/2,886.5 | 4,137.0/4,116.9/4,341.8 |
| ml-dsa | ML-DSA-44 | verify_valid | 1000/1000/1000 | 66.3 | 66% | 125.5 | 21% | 54.9 | 66.3/59.5/103.2 | 128.8/101.8/125.5 |
| ml-dsa | ML-DSA-44 | verify_invalid_msg | 1000/1000/1000 | 81.5 | 75% | 136.4 | 27% | 56.2 | 81.5/59.8/120.8 | 136.4/101.1/138.3 |
| ml-dsa | ML-DSA-44 | verify_invalid_flip | 1000/1000/1000 | 60.9 | 92% | 127.3 | 27% | 54.6 | 58.2/60.9/114.0 | 101.3/127.3/135.6 |
| ml-dsa | ML-DSA-65 | keygen | 1000/1000/1000 | 242.3 | 10% | 532.6 | 6% | 195.1 | 242.3/231.9/256.6 | 547.1/514.2/532.6 |
| ml-dsa | ML-DSA-65 | sign | 1000/1000/1000 | 2,405.9 | 5% | 4,434.0 | 3% | 1,728.2 | 2,290.7/2,414.3/2,405.9 | 4,434.0/4,533.0/4,379.5 |
| ml-dsa | ML-DSA-65 | verify_valid | 1000/1000/1000 | 94.5 | 17% | 183.7 | 4% | 82.6 | 94.5/92.4/108.3 | 189.0/182.4/183.7 |
| ml-dsa | ML-DSA-65 | verify_invalid_msg | 1000/1000/1000 | 91.5 | 10% | 175.7 | 15% | 80.4 | 91.5/89.7/99.1 | 187.5/161.1/175.7 |
| ml-dsa | ML-DSA-65 | verify_invalid_flip | 1000/1000/1000 | 85.9 | 46% | 153.6 | 19% | 76.4 | 85.9/83.4/123.2 | 153.6/176.7/148.3 |
| ml-dsa | ML-DSA-87 | keygen | 1000/1000/1000 | 405.1 | 5% | 839.5 | 5% | 318.8 | 405.1/407.1/388.6 | 809.7/839.5/848.0 |
| ml-dsa | ML-DSA-87 | sign | 1000/1000/1000 | 2,607.7 | 6% | 5,007.1 | 2% | 1,954.0 | 2,494.9/2,607.7/2,663.0 | 4,938.9/5,039.4/5,007.1 |
| ml-dsa | ML-DSA-87 | verify_valid | 1000/1000/1000 | 142.6 | 17% | 273.4 | 20% | 107.0 | 118.5/142.6/143.2 | 224.3/277.9/273.4 |
| ml-dsa | ML-DSA-87 | verify_invalid_msg | 1000/1000/1000 | 134.7 | 11% | 266.4 | 2% | 111.0 | 134.7/133.6/147.8 | 266.4/268.8/263.8 |
| ml-dsa | ML-DSA-87 | verify_invalid_flip | 1000/1000/1000 | 140.8 | 18% | 272.2 | 5% | 112.6 | 152.8/128.1/140.8 | 261.7/272.2/276.5 |
| fips205 | SLH-DSA-SHAKE-128s | keygen | 99/108/107 | 269,964.7 | 7% | 407,691.0 | 6% | 201,754.7 | 288,353.0/269,964.7/268,658.3 | 411,919.2/388,573.9/407,691.0 |
| fips205 | SLH-DSA-SHAKE-128s | sign | 30/30/30 | 2,124,520.5 | 50% | 2,906,864.1 | 138% | 1,770,487.3 | 3,068,579.1/2,002,949.5/2,124,520.5 | 6,439,666.6/2,439,750.9/2,906,864.1 |
| fips205 | SLH-DSA-SHAKE-128s | verify_valid | 1000/1000/1000 | 1,792.5 | 83% | 3,345.4 | 10% | 1,360.4 | 3,050.4/1,571.5/1,792.5 | 3,345.4/3,023.2/3,355.7 |
| fips205 | SLH-DSA-SHAKE-128s | verify_invalid_msg | 1000/1000/1000 | 2,009.9 | 60% | 3,411.4 | 18% | 1,402.1 | 2,763.2/2,009.9/1,550.6 | 3,411.4/3,670.3/3,049.8 |
| fips205 | SLH-DSA-SHAKE-128s | verify_invalid_flip | 1000/1000/1000 | 1,791.0 | 81% | 3,152.6 | 12% | 1,307.3 | 3,019.9/1,791.0/1,569.5 | 3,457.8/3,064.6/3,152.6 |
| fips205 | SLH-DSA-SHAKE-128f | keygen | 1000/1000/1000 | 3,527.7 | 62% | 6,630.1 | 9% | 2,689.8 | 5,305.8/3,527.7/3,134.9 | 6,934.4/6,630.1/6,369.2 |
| fips205 | SLH-DSA-SHAKE-128f | sign | 249/304/332 | 98,662.8 | 32% | 144,211.6 | 18% | 64,499.5 | 122,207.6/98,662.8/90,563.6 | 154,162.7/144,211.6/128,196.9 |
| fips205 | SLH-DSA-SHAKE-128f | verify_valid | 1000/1000/1000 | 6,228.3 | 63% | 10,007.6 | 23% | 4,053.4 | 8,543.4/6,228.3/4,632.1 | 11,478.1/10,007.6/9,130.6 |
| fips205 | SLH-DSA-SHAKE-128f | verify_invalid_msg | 1000/1000/1000 | 5,171.5 | 74% | 9,233.5 | 28% | 3,810.7 | 8,375.2/5,171.5/4,572.9 | 11,438.6/9,233.5/8,825.0 |
| fips205 | SLH-DSA-SHAKE-128f | verify_invalid_flip | 1000/1000/1000 | 5,424.6 | 46% | 9,622.0 | 24% | 3,819.9 | 7,764.2/5,424.6/5,271.4 | 11,742.8/9,622.0/9,392.3 |
| fips205 | SLH-DSA-SHAKE-192f | keygen | 1000/1000/1000 | 5,386.6 | 81% | 9,523.7 | 128% | 3,844.8 | 9,380.5/5,386.6/4,996.3 | 21,658.1/9,523.7/9,452.8 |
| fips205 | SLH-DSA-SHAKE-192f | sign | 157/196/187 | 153,799.0 | 17% | 236,992.4 | 111% | 113,428.0 | 175,559.2/149,974.3/153,799.0 | 485,319.8/223,134.6/236,992.4 |
| fips205 | SLH-DSA-SHAKE-192f | verify_valid | 1000/1000/1000 | 10,953.4 | 52% | 18,864.8 | 84% | 5,739.1 | 13,265.6/7,541.8/10,953.4 | 29,483.1/13,591.6/18,864.8 |
| fips205 | SLH-DSA-SHAKE-192f | verify_invalid_msg | 1000/1000/1000 | 7,613.7 | 56% | 13,536.8 | 42% | 5,431.2 | 11,630.4/7,613.7/7,372.7 | 19,089.8/13,352.1/13,536.8 |
| fips205 | SLH-DSA-SHAKE-192f | verify_invalid_flip | 1000/1000/1000 | 7,206.3 | 7% | 13,112.9 | 3% | 5,150.5 | 7,495.3/7,206.3/6,993.1 | 13,367.1/13,112.9/12,909.0 |
| fips204 | ML-DSA-44 | keygen | 1000/1000/1000 | 122.5 | 63% | 220.8 | 11% | 96.6 | 122.5/111.3/188.0 | 220.8/201.5/226.5 |
| fips204 | ML-DSA-44 | sign | 1000/1000/1000 | 354.5 | 2% | 1,899.7 | 12% | 118.8 | 356.1/349.3/354.5 | 1,988.4/1,769.8/1,899.7 |
| fips204 | ML-DSA-44 | verify_valid | 1000/1000/1000 | 73.7 | 5% | 141.4 | 18% | 65.2 | 72.5/73.7/76.2 | 127.9/141.4/153.0 |
| fips204 | ML-DSA-44 | verify_invalid_msg | 1000/1000/1000 | 77.9 | 15% | 153.4 | 12% | 63.4 | 84.4/73.0/77.9 | 153.4/135.1/154.0 |
| fips204 | ML-DSA-44 | verify_invalid_flip | 1000/1000/1000 | 73.4 | 4% | 137.5 | 10% | 64.8 | 73.0/76.1/73.4 | 137.5/149.9/135.5 |
| fips204 | ML-DSA-65 | keygen | 1000/1000/1000 | 173.9 | 0% | 350.7 | 1% | 145.1 | 173.8/173.9/174.4 | 351.2/350.7/349.3 |
| fips204 | ML-DSA-65 | sign | 1000/1000/1000 | 569.3 | 47% | 2,830.4 | 43% | 164.3 | 777.8/512.1/569.3 | 3,637.3/2,433.0/2,830.4 |
| fips204 | ML-DSA-65 | verify_valid | 1000/1000/1000 | 215.7 | 56% | 264.9 | 10% | 117.6 | 252.0/215.7/130.7 | 289.0/261.3/264.9 |
| fips204 | ML-DSA-65 | verify_invalid_msg | 1000/1000/1000 | 133.5 | 96% | 255.7 | 13% | 114.1 | 251.5/133.5/123.3 | 282.0/255.7/249.2 |
| fips204 | ML-DSA-65 | verify_invalid_flip | 1000/1000/1000 | 122.9 | 95% | 257.9 | 15% | 108.6 | 236.2/119.7/122.9 | 275.9/236.9/257.9 |
| fips204 | ML-DSA-87 | keygen | 1000/1000/1000 | 367.0 | 62% | 566.6 | 8% | 252.2 | 543.8/316.1/367.0 | 606.5/563.7/566.6 |
| fips204 | ML-DSA-87 | sign | 1000/1000/1000 | 741.6 | 51% | 4,073.9 | 40% | 278.5 | 1,028.2/651.5/741.6 | 4,458.1/2,834.8/4,073.9 |
| fips204 | ML-DSA-87 | verify_valid | 1000/1000/1000 | 205.7 | 110% | 429.5 | 10% | 188.3 | 429.4/205.7/202.2 | 469.6/429.5/425.3 |
| fips204 | ML-DSA-87 | verify_invalid_msg | 1000/1000/1000 | 204.1 | 108% | 419.5 | 14% | 182.2 | 418.9/198.5/204.1 | 461.9/402.9/419.5 |
| fips204 | ML-DSA-87 | verify_invalid_flip | 1000/1000/1000 | 241.5 | 81% | 448.8 | 9% | 183.7 | 425.4/230.6/241.5 | 465.6/425.4/448.8 |

Columns: `n per run` are the three runs' sample counts. `p50` and `p99` are the medians of the three runs' values in microseconds. Spread is (max - min) / median. The last two columns list the three runs separately, as `docs/perf.md` section 4 requires.

**Tolerance check (`docs/perf.md` section 4).** The contract asks the three runs to agree within 10 percent (p50) and 20 percent (p99). Most rows do not. The noise source is known: the host was shared and busy (section 2). The contract's next step is to fix the setup and rerun; that needs a quiet, dedicated host, which the owner has not assigned (`docs/perf.md` section 9, item 1). Until then treat every p50 and p99 below as indicative within a factor that the spread column states, and treat the sizes as exact. The `min` column (the fastest of the samples in each run, median over the runs) is less sensitive to contention and is given as a floor.

10 of 51 rows within tolerance; 41 over:

- fips204/ML-DSA-44/keygen (p50 63%, p99 11%)
- fips204/ML-DSA-44/verify_invalid_msg (p50 15%, p99 12%)
- fips204/ML-DSA-65/sign (p50 47%, p99 43%)
- fips204/ML-DSA-65/verify_invalid_flip (p50 95%, p99 15%)
- fips204/ML-DSA-65/verify_invalid_msg (p50 96%, p99 13%)
- fips204/ML-DSA-65/verify_valid (p50 56%, p99 10%)
- fips204/ML-DSA-87/keygen (p50 62%, p99 8%)
- fips204/ML-DSA-87/sign (p50 51%, p99 40%)
- fips204/ML-DSA-87/verify_invalid_flip (p50 81%, p99 9%)
- fips204/ML-DSA-87/verify_invalid_msg (p50 108%, p99 14%)
- fips204/ML-DSA-87/verify_valid (p50 110%, p99 10%)
- fips205/SLH-DSA-SHAKE-128f/keygen (p50 62%, p99 9%)
- fips205/SLH-DSA-SHAKE-128f/sign (p50 32%, p99 18%)
- fips205/SLH-DSA-SHAKE-128f/verify_invalid_flip (p50 46%, p99 24%)
- fips205/SLH-DSA-SHAKE-128f/verify_invalid_msg (p50 74%, p99 28%)
- fips205/SLH-DSA-SHAKE-128f/verify_valid (p50 63%, p99 23%)
- fips205/SLH-DSA-SHAKE-128s/sign (p50 50%, p99 138%)
- fips205/SLH-DSA-SHAKE-128s/verify_invalid_flip (p50 81%, p99 12%)
- fips205/SLH-DSA-SHAKE-128s/verify_invalid_msg (p50 60%, p99 18%)
- fips205/SLH-DSA-SHAKE-128s/verify_valid (p50 83%, p99 10%)
- fips205/SLH-DSA-SHAKE-192f/keygen (p50 81%, p99 128%)
- fips205/SLH-DSA-SHAKE-192f/sign (p50 17%, p99 111%)
- fips205/SLH-DSA-SHAKE-192f/verify_invalid_msg (p50 56%, p99 42%)
- fips205/SLH-DSA-SHAKE-192f/verify_valid (p50 52%, p99 84%)
- k256/ECDSA-secp256k1/keygen (p50 13%, p99 14%)
- k256/ECDSA-secp256k1/recover (p50 28%, p99 2%)
- k256/ECDSA-secp256k1/sign (p50 21%, p99 22%)
- k256/ECDSA-secp256k1/verify_invalid_flip (p50 14%, p99 10%)
- k256/ECDSA-secp256k1/verify_invalid_msg (p50 30%, p99 14%)
- k256/ECDSA-secp256k1/verify_valid (p50 14%, p99 10%)
- ml-dsa/ML-DSA-44/sign (p50 36%, p99 5%)
- ml-dsa/ML-DSA-44/verify_invalid_flip (p50 92%, p99 27%)
- ml-dsa/ML-DSA-44/verify_invalid_msg (p50 75%, p99 27%)
- ml-dsa/ML-DSA-44/verify_valid (p50 66%, p99 21%)
- ml-dsa/ML-DSA-65/keygen (p50 10%, p99 6%)
- ml-dsa/ML-DSA-65/verify_invalid_flip (p50 46%, p99 19%)
- ml-dsa/ML-DSA-65/verify_invalid_msg (p50 10%, p99 15%)
- ml-dsa/ML-DSA-65/verify_valid (p50 17%, p99 4%)
- ml-dsa/ML-DSA-87/verify_invalid_flip (p50 18%, p99 5%)
- ml-dsa/ML-DSA-87/verify_invalid_msg (p50 11%, p99 2%)
- ml-dsa/ML-DSA-87/verify_valid (p50 17%, p99 20%)

## 5. Notes on the numbers

Observations on this host and these builds. They describe the table above. They are not conclusions about security or about which scheme to choose.

1. **A bad signature costs about as much to reject as a good one to accept.** For every scheme, `verify_invalid_msg` and `verify_invalid_flip` are within the noise of `verify_valid`. Examples (p50, us): ML-DSA-65 (`ml-dsa`) 94.5 valid, 91.5 and 85.9 invalid; ECDSA 68.7 valid, 70.7 and 69.6 invalid; SLH-DSA-128f 6,228 valid, 5,172 and 5,425 invalid. The two invalid kinds tested do not take a cheap early exit. A malformed encoding that a decoder rejects at once (for example an out-of-range hint count in an ML-DSA signature) was not tested and may be cheaper. The relevance to flooding is for Track B and the txpool owner: an attacker can send signatures that cost the verifier a full verification each.
2. **ECDSA baseline.** `verify` is 68.7 us and `recover` is 153.3 us (2.2 times). The chain uses recovery. `k256` was built with the workspace features (`ecdsa` only, no precomputed tables); the crate's default features include precomputed tables and would verify faster.
3. **ML-DSA verification is close to ECDSA.** ML-DSA-44, -65 and -87 verify in 66.3, 94.5 and 142.6 us (`ml-dsa`), against 68.7 (verify) and 153.3 (recover) for ECDSA. Verify cost is not where ML-DSA is expensive; size is (section 3).
4. **ML-DSA signing is slower and uneven.** `ml-dsa 0.1.2` signs in 2.2 to 2.6 ms p50, `fips204 0.5.0` in 0.35 to 0.74 ms, ECDSA in 62 us. The two libraries differ three- to six-fold on the same loop; the cause was not investigated. The p99 is 1.8 to 5.5 times the p50 because ML-DSA signing repeats a rejection loop a random number of times (FIPS 204 Table 1: about 4 to 5 repetitions on average).
5. **Library variance on verify.** For ML-DSA-65 the two libraries give 94.5 us (`ml-dsa`) and 215.7 us (`fips204`) at p50, but 82.6 and 117.6 us at the minimum. The `fips204` p50 spread across runs is 56 percent. The host noise is larger than most differences between the libraries; do not rank the libraries from this table.
6. **SLH-DSA in pure Rust, no SIMD.** SHAKE-128s signs in about 2.1 s and generates a key in about 270 ms; SHAKE-128f signs in about 99 ms and SHAKE-192f in about 154 ms. Verification takes 1.8 ms (128s), 6.2 ms (128f) and 11.0 ms (192f). No SIMD hashing was built, so these numbers say nothing about optimised implementations.
7. **Sample counts.** SLH-DSA-128s signing has n = 30 per run, so its p99 is the maximum of 30 values. SLH-DSA-128s key generation has about 100 samples. SLH-DSA sign for 128f and 192f has 157 to 332 samples.
8. **Noise.** 41 of 51 rows are over the contract tolerance (section 4). The largest p50 spreads (108 to 110 percent) are in `fips204` ML-DSA-87 verify; SLH-DSA rows reach 83 percent. Spreads of up to 30 percent also occur for ECDSA. This is the host, not the schemes.

## 6. Cross-check of the percentile code

The percentile code in `pq-lab` was checked against `bench/collect/summarise.py` on the raw per-sample CSV of every run: for all 153 (run, scheme, operation) rows, `n`, `p50`, `p95`, `p99`, `p99.9` and `max` from the Rust code equal the `summarise.py` output to 1e-9 ms (the CSV values were converted from ns to ms and passed as the `latency_ms` column). The unit test `pct_is_nearest_rank_like_summarise_py` checks the rule on known inputs, including the case where p99.9 equals the maximum for fewer than 1,000 samples.

## 7. Reproduce

```sh
cargo build --release -p pq-lab
taskset -c <cpu> target/release/pq-lab --csv samples.csv > run.jsonl        # about 5 to 10 minutes
bash bench/collect/env.sh --datadir <dir> --binary target/release/pq-lab --pid <pid> > env.json
python3 -I labs/pq/model.py results run1.jsonl run2.jsonl run3.jsonl        # the table in section 4
```

Options: `--only <substring of "impl/scheme">`, `--min-n`, `--max-n`, `--budget-s`, `--warmup`, `--csv`, `--version`. The no-ship test is `cargo test --locked -p pq-lab --test no_ship`.

## 8. Not covered

- Message sizes other than 32 bytes. No ML-KEM. No stateful scheme (LMS, XMSS). SLH-DSA-192s, the 256-bit SLH-DSA sets and the SHA2 instances were not measured. No `slh-dsa` (RustCrypto), `libcrux` or FFI library was measured (`implementations.md`).
- No AVX2, NEON or other SIMD path: the pure-Rust libraries ran as built, with default flags.
- No cold-cache, no soak, no open-loop load and no multi-thread scaling.
- No energy, memory high-water mark or stack use. ML-DSA needs a large stack in unoptimised builds (the unit test runs in a 64 MiB thread for that reason).
- No timing-side-channel measurement of any kind. These are throughput numbers.
