# Process

| Document | Purpose |
|---|---|
| [engineering-philosophy.md](engineering-philosophy.md) | xOneRS against Lambda Class's engineering philosophy: ratings, evidence, issues |
| [ponytail-audit.md](ponytail-audit.md) | Whole-repo over-engineering audit: what to delete, reuse or replace |

## PR rule: the ponytail ladder

Every change applies the ladder of [ponytail](https://github.com/dietrichgebert/ponytail) (`skills/ponytail/SKILL.md`). Read the task and the code it touches first. Then stop at the first rung that holds:

1. Does it need to exist? A speculative need is not a need.
2. Is it already in the codebase? Reuse the helper, type or pattern.
3. Does `std` do it?
4. Does the platform do it (OS, MDBX, the database or protocol feature)?
5. Does a dependency that is already in `Cargo.lock` do it? Do not add a dependency for a few lines.
6. Can it be one line?
7. Only then: the minimum code that works.

No trait with one implementation, no config for a value that never changes, no scaffolding for later. Exceptions: a consensus rule, a gate of the gate DAG (#5, #57) or an accepted interface item (for example #17, #40). Name the item in the PR.

A review may cite the ladder: "ponytail rung 3: `std::time::SystemTime` covers this". The author answers with the change or with the reason the rung does not hold.

The ladder does not shorten validation at trust boundaries, error handling that prevents data loss, signer safety, or tests. Risky logic keeps one test that fails when it breaks.

## `ponytail:` comments

Mark a deliberate shortcut with a known limit in the code, at the shortcut:

```rust
// ponytail: one global lock on the pool; per-sender locks if admission shows up in a profile.
```

The comment names the limit and the upgrade path. A review and an audit treat a marked shortcut as a decision, not as a finding, until the expected load crosses the limit. To list them: `grep -rn 'ponytail:' crates bin`.
