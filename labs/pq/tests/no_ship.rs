//! Nothing from the PQ lab may enter a shipping build (issue #18). The shipping binary is
//! `bin/xone`; its dependency tree must contain neither the lab crate nor any PQ library,
//! and no workspace member may depend on the lab.

use std::process::Command;

/// Crate-name prefixes of the lab and of every PQ library registered in docs/pq/implementations.md.
const FORBIDDEN: &[&str] = &[
    "pq-lab", "ml-dsa", "ml-kem", "slh-dsa", "fips203", "fips204", "fips205", "pqcrypto", "oqs",
    "libcrux", "pqc_",
];

/// Crate names (without versions) printed by `cargo tree`, one per line.
fn tree(args: &[&str]) -> Vec<String> {
    let out = Command::new(env!("CARGO"))
        .args(["tree", "--locked", "--prefix", "none", "--format", "{p}"])
        .args(args)
        .output()
        .expect("cargo tree runs");
    assert!(
        out.status.success(),
        "cargo tree {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    text.lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(String::from)
        .collect()
}

fn forbidden(names: &[String]) -> Vec<&String> {
    names
        .iter()
        .filter(|n| FORBIDDEN.iter().any(|p| n.starts_with(p)))
        .collect()
}

#[test]
fn shipping_binary_has_no_lab_or_pq_crate() {
    for edges in ["normal", "normal,build"] {
        let names = tree(&["-p", "xone", "--edges", edges]);
        assert!(
            names.iter().any(|n| n == "xone"),
            "tree for xone is empty: {names:?}"
        );
        assert_eq!(forbidden(&names), Vec::<&String>::new(), "edges {edges}");
    }
}

/// Positive control: the check above is not vacuous, the lab tree does contain PQ crates.
#[test]
fn control_lab_tree_contains_pq_crates() {
    let names = tree(&["-p", "pq-lab", "--edges", "normal"]);
    for want in ["pq-lab", "ml-dsa", "fips204", "fips205"] {
        assert!(
            names.iter().any(|n| n == want),
            "{want} missing from the lab tree"
        );
    }
}

/// No workspace member (any edge kind) depends on the lab: its inverse tree names only the lab
/// (cargo also prints a "feature default" line for it, hence all() and not equality).
#[test]
fn nothing_depends_on_the_lab() {
    let names = tree(&["--workspace", "--edges", "all", "-i", "pq-lab"]);
    assert!(
        !names.is_empty() && names.iter().all(|n| n == "pq-lab"),
        "{names:?}"
    );
}
