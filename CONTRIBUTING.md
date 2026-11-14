# Contributing to xOneRS

## Branches and tags

- One long-lived branch: `main`. There is no `master`, `develop` or release branch.
- Release tags (`vX.Y.Z`) go on `main`, on the merge commit of the version bump. The tag must equal `v<[workspace.package] version>`. See `docs/ops/release.md`.
- Work happens on a short fix branch from `main`, one branch for one issue slice, merged by pull request.
- A fix branch is merged or closed within 14 days. A branch with no merge after 14 days is closed, and its commit stays reachable from the pull request.

## Pull requests

- Say `Refs #n` in a pull request that leaves rows of the issue open. Say `Closes #n` in the pull request that makes the last row hold.
- Show the evidence for each acceptance row: a test, a vector run or a report, linked or pasted in the body.
- Run the full gate once on the final commit (`cargo fmt --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace`). While GitHub Actions is on hold, put the local log in the pull request body.
- Apply the ponytail ladder in `docs/process/README.md`. Mark a deliberate shortcut with a `ponytail:` comment.

## Commits

- Author is the person who wrote the change. Co-author trailers are the maintainers' to set.
- No AI or tool attribution in a commit message or a pull request body.
- No force-push to a branch that has a pull request open, and never to `main`.

## Version stamp

`xone --version` is the one version string: `xOneRS/v<semver>-<commit8>[-dirty]-<DDMMYYYY>/<os>-<arch>/rustc<version>`. The p2p client name, `web3_clientVersion`, the ethstats `node` and `client` fields and the `xone_build_info` label read the same string. The word `unknown` must not appear in it. A release build refuses a stamp with `unknown` or `-dirty`.
