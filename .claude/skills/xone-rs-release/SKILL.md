---
name: xone-rs-release
description: Build stamping, the pull-request flow, commit identity and release steps for XDCIndia/xOne. Use when committing, opening a PR, tagging, building a release binary, publishing a release, or when the task says commit, push, PR, version stamp, release, tag, changelog, co-author, trailers.
---

# xone-rs-release

## Operating rules

1. **Commit identity.** Author and committer: `AnilChinchawale <29918376+AnilChinchawale@users.noreply.github.com>`.
2. **Trailers.** Every commit ends with exactly these three lines and no other trailer:

   ```
   Co-authored-by: AnilChinchawale <29918376+AnilChinchawale@users.noreply.github.com>
   Co-authored-by: AnilChinchawaleXDC <270199611+AnilChinchawaleXDC@users.noreply.github.com>
   Co-authored-by: MestryOmkar <36405230+MestryOmkar@users.noreply.github.com>
   ```

3. **No tool or AI attribution** in commits, PR bodies, issues, comments or release notes. No "generated with" line. If a tool adds one, remove it before the commit.
4. **One default branch: `main`.** Work goes to `main` through PRs; releases are cut from `main`. There is no second long-lived branch to sync.
5. **No direct push to `main`.** One work item, one branch, one PR.
6. **The owner merges and the owner releases** unless the owner has delegated it for that PR or tag.
7. XDCIndia repositories only. Nothing on XinFinOrg.
8. No binaries, datadirs, keys or keystores in the repository.

## Version stamp

Format: `xOne/v<semver>-<commit8>-<DDMMYYYY>/<os>-<arch>/rustc<version>`

- `<commit8>` is the first 8 characters of the commit; a modified tree adds `-dirty`.
- `<DDMMYYYY>` is the UTC build date.
- The stamp is set at build time (a `build.rs` in `xone-node`), and one function returns it. `web3_clientVersion`, the P2P hello, ethstats and `xone --version` all call that function.
- A build with no git data reports `unknown`, never an empty or stale value.
- Release binaries are built with `cargo auditable`, so the dependency list is inside the binary.

## Commit message

`<crate(s)>: short lowercase description`, for example `xone-consensus: read the v2 config by round`. Body: what and why, the spec page, the differential result.

## PR flow

1. Branch from `main`: `feat/…`, `fix/…`, `chore/…`.
2. Local gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run`, `cargo deny check`, `cargo audit`, the layering check, miri on primitives and consensus crates.
3. Consensus-critical PRs (`xone-chainspec`, `xone-consensus`, `xone-evm`, `xone-state`) show the spec page and the differential result (range, 0 mismatches, commit, pin, host).
4. PR title follows the commit format. The body states what, why, proof, tests, and what is not covered.
5. Push with the account that has push rights, then open the PR as `AnilChinchawale`:

   ```
   gh auth switch --user AnilChinchawaleXDC && git push -u origin <branch>
   gh auth switch --user AnilChinchawale && gh pr create -R XDCIndia/xOne --base main
   ```

6. Do not merge your own PR.

## Release steps

1. `main` is green and the milestone gate that the release claims is recorded.
2. Bump the workspace version; update `CHANGELOG.md`; PR; merge.
3. Tag `vX.Y.Z` on the merge commit.
4. Build `linux-amd64` with `cargo auditable build --release --locked` on a clean checkout of the tag.
5. Check `xone --version` shows the tag, the commit and today's date, and no `-dirty`.
6. Write `SHA256SUMS`. Create the GitHub release with the binary, the sums and notes: what changed, proof on a live network (ranges, seals), known gaps.
7. A node that runs the release must report the same stamp on SkyNet.

## Done

`git log -1 --format='%an <%ae>%n%cn <%ce>%n%b'` shows the identity and exactly three trailers; the PR is open and not merged by its author; for a release, the stamp on a running node equals the tag and commit.
