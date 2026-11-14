# Release (XB-26, issue #46)

GitHub Actions is on a billing hold, so every step runs locally with `ci/release.sh`; the commands and
their output are recorded in `docs/ops/release-rehearsal.md`. A CI job later runs the same script.

## Cut a release

1. `main` is green, the gate the release claims is recorded, the workspace version and `CHANGELOG.md` are bumped by PR, merged.
2. The owner tags `vX.Y.Z` on the merge commit. The tag must equal `v<[workspace.package] version>`.
3. On a clean checkout of the tag: `XONE_RELEASE_KEY=<key id> ci/release.sh` (or `make release`). Output in `dist/`:

| File | What |
|---|---|
| `xone-<v>-linux-<arch>-musl.tar.gz` (x86_64, aarch64) | static `xone`, `run-xone.sh`, `xone@.service`, `bootnodes/`, `LICENSE`; fixed file order, owner, mtime and gzip header |
| `xone-<v>.cdx.json` | SBOM, CycloneDX 1.5 (`cargo cyclonedx` 0.5.10) |
| `xone-<v>.provenance.json` | in-toto Statement v1, SLSA provenance v1 shape: builder id, commit, tag, toolchain, start and finish time, `SOURCE_DATE_EPOCH`, RUSTFLAGS, hashes of `Cargo.lock`, `rust-toolchain.toml` and `ci/release.sh`, subject digests, warnings |
| `SHA256SUMS` | over every file above |
| `SHA256SUMS.sig` | detached gpg signature of `SHA256SUMS` |

Binaries are static (`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`): no libc on the host is
needed, so they start on Ubuntu 20.04 and later and Debian 11 and later. The script builds with
`cargo zigbuild` (zig is the cross linker for both arches; set `XONE_TARGETS` to build fewer). Setup on the build host:
`rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl`, `cargo install cargo-zigbuild --locked`,
and `pip install ziglang` with a `zig` wrapper on `PATH` (`exec python3 -m ziglang "$@"`). The script also refuses a
binary that is not static, has any `GLIBC` symbol (`objdump -T`), or is larger than the size budget
(`XONE_SIZE_BUDGET`, default 18 MiB, the first musl build of 15.0 MB plus 20 percent, stripped; `ci/check-release-tree.sh` checks the same number). Record the
stripped sizes (printed as `release: sizes ...` and stored under `xone:binarySizes` in the provenance file) in the release notes.
Manual check of a tarball: `ldd xone` says "not a dynamic executable"; `objdump -T xone | grep GLIBC` prints nothing;
`docker run --rm -v $PWD:/x ubuntu:22.04 /x/xone --version`.

Not done yet (issue #193): a `FROM scratch` image per arch, `mimalloc` for the musl allocator, the `transfers` bench
comparison with the glibc build (10 percent limit), and the distro matrix with a 1,000-block import.

`make docker` wraps the checked tarball in an image (`Dockerfile`, base pinned by digest, uid 10001).
The script refuses: a dirty tree; HEAD not at a tag; a tag that is not `v<workspace version>`; a stamp with
`unknown` or `-dirty`; a stamp that differs from tag, commit and commit date; a missing key; a build that
dirties the tree. The stamp date is the commit date (`SOURCE_DATE_EPOCH`), so the stamp is the same on
every rebuild of the tag.

Operators verify as in `ops/install.md` steps 2 to 5. The binary also carries its dependency list
(`cargo audit bin /opt/xone/xone`, section `.dep-v0`).

## Tool choices

| Need | Choice | Why |
|---|---|---|
| Signature | `gpg --detach-sign` | present on every Ubuntu host (`gpg` 2.4); `minisign` is not installed here and has no offline install path; gpg also needs no new dependency |
| Dependency list in the binary | `cargo auditable` 0.7.7, installed `--locked` | plain `cargo build` fallback with a warning in the provenance file if the install fails |
| SBOM | `cargo cyclonedx` 0.5.10, installed `--locked` | fallback is `cargo tree --locked` text, recorded as a gap in the provenance file |
| Timestamps and ordering | `SOURCE_DATE_EPOCH` = commit time; `tar --sort=name --mtime`, `gzip -n` | reproducible tarball |

## Release key: owner decision (open)

No release key exists. The tests use a throwaway key made in a temp keyring and never keep it. The owner
decides: where the key lives (hardware token or offline host is recommended; not on a CI runner without a
protected, separate signing job), who may use it, rotation and revocation, and the key id to publish in
the crypto inventory (`docs/pq/inventory.md`, #17). This is a classical signature; the post-quantum path
is in track Q. Until then the owner, not this repository, supplies `XONE_RELEASE_KEY`. Mainnet signing
approval is out of scope (non-goal of #46). The key never goes to CI logs: sign in a separate step that
receives only `SHA256SUMS`.

## Reproducibility

`RUSTFLAGS="--remap-path-prefix=<checkout>=/build --remap-path-prefix=<CARGO_HOME>=/cargo --remap-path-prefix=<sysroot>=/rustc -C link-arg=-Wl,--build-id=none"`,
`--locked`, `SOURCE_DATE_EPOCH`. Result and method: `docs/ops/release-rehearsal.md` section 2. Not yet
shown: two builders on separate machines with different users and home directories (issue #46 gate
"two builders reproduce the hash"). The same toolchain (`rust-toolchain.toml`, 1.94.0) and the same zig version are required. The host libc no longer
matters: the binary is static.

## Not covered here

CI on a tag (`.github/workflows/release.yml`, blocked by the billing hold); a run of the aarch64 binary (it is built and stamp-checked from its strings; no arm host or qemu here); the
two-machine hash; the 15-minute timed install by a second person; the public installer (`one.sh`)
decision; the release key; image push and image signing; SBOM and provenance signing beyond the
`SHA256SUMS` signature.
