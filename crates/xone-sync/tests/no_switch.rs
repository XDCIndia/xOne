#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods
)]
//! The receipts-root and body-versus-header checks run on every imported block and no switch can turn
//! them off. This is a source check (a grep), next to the behavioural tests in `chain.rs`
//! (`every_check_runs_on_every_import`, `header_check_runs_before_anything_is_stored`).

const INSERT: &str = include_str!("../src/chain/insert.rs");
const SOURCES: [(&str, &str); 14] = [
    ("lib.rs", include_str!("../src/lib.rs")),
    ("executor.rs", include_str!("../src/executor.rs")),
    ("store.rs", include_str!("../src/store.rs")),
    ("chain/mod.rs", include_str!("../src/chain/mod.rs")),
    ("chain/insert.rs", INSERT),
    (
        "chain/fork_choice.rs",
        include_str!("../src/chain/fork_choice.rs"),
    ),
    ("chain/reorg.rs", include_str!("../src/chain/reorg.rs")),
    ("chain/resume.rs", include_str!("../src/chain/resume.rs")),
    (
        "chain/set_head.rs",
        include_str!("../src/chain/set_head.rs"),
    ),
    (
        "chain/side_index.rs",
        include_str!("../src/chain/side_index.rs"),
    ),
    ("testkit/mod.rs", include_str!("../src/testkit/mod.rs")),
    ("testkit/mem.rs", include_str!("../src/testkit/mem.rs")),
    ("testkit/exec.rs", include_str!("../src/testkit/exec.rs")),
    ("Cargo.toml", include_str!("../Cargo.toml")),
];

#[test]
fn body_and_receipt_verification_is_unconditional_in_the_import_path() {
    let body = INSERT.split("fn insert_locked").nth(1).unwrap();
    let pre = "verifier.verify_body(&block.header, &block.body)?;";
    let post =
        "verifier.verify_receipts(&block.header, block.body.transactions.len(), &ex.receipts)?;";
    let (pre_at, post_at) = (
        body.find(pre).expect("body check"),
        body.find(post).expect("receipt check"),
    );
    // The body is checked against the header before anything is executed.
    assert!(pre_at < body.find("self.exec.execute(").unwrap());
    // From the known-record exit to the body check: no branch can skip it.
    let before = &body[..pre_at];
    let from = before
        .rfind("let known = ")
        .expect("the known-record path ends before the check");
    let tail = &before[from..];
    let tail = &tail[tail.find("return self.settle(rec, true);").unwrap()..];
    let tail: String = tail
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    for word in ["if ", "match ", "else", "cfg", "while", "for "] {
        assert!(
            !tail.contains(word),
            "`{word}` between the known-record exit and the body check"
        );
    }
    // From the execution result to the receipt check: a straight line, no branch, no early return.
    let from = body[..post_at]
        .rfind("let ex = executed?;")
        .expect("verification follows execution");
    let between = body[from..post_at].replace("executed?;", "");
    for word in [
        "if ", "match ", "return", "?.", "else", "cfg", "while", "for ",
    ] {
        assert!(
            !between.contains(word),
            "`{word}` between execution and the receipt check"
        );
    }
    // The block is committed or stored only after the receipt check and the three comparisons.
    let tail = &body[post_at..];
    let gas = tail.find("ex.gas_used != block.header.gas_used").unwrap();
    let bal = tail
        .find("ex.access_list_hash != block.header.block_access_list_hash")
        .unwrap();
    let roots = tail
        .find("ex.state_root != block.header.state_root")
        .unwrap();
    let settle = tail.find("self.settle(").unwrap();
    assert!(gas < bal && bal < roots && roots < settle);
    // `settle` is reached from `insert_locked` only through this path or the known-record path.
    assert_eq!(INSERT.matches("self.settle(").count(), 2);
    // The header check runs in `insert`, before the lock, with no condition.
    let insert = INSERT
        .split("pub fn insert")
        .nth(1)
        .unwrap()
        .split("fn insert_locked")
        .next()
        .unwrap();
    assert!(insert.contains(".check(&self.reader(), &block.header, &parent, now)?;"));
    assert!(insert.find(".check(").unwrap() < insert.find("lock_import").unwrap());
}

#[test]
fn nothing_in_the_crate_can_switch_a_check_off() {
    let banned = [
        "env::var",
        "cfg(feature",
        "cfg(not",
        "cfg!(",
        "skip_verify",
        "skip_body",
        "no_verify",
        "unchecked",
        "disable_",
        "bypass",
        "trusted",
        "lenient",
        "insecure",
    ];
    for (file, text) in SOURCES {
        let code: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for w in banned {
            assert!(!code.contains(w), "{file}: `{w}`");
        }
    }
    let manifest = SOURCES[13].1;
    assert!(
        !manifest.contains("[features]"),
        "a Cargo feature could switch a check"
    );
}
