//! The consensus corpus: every valid vector is accepted, every negative vector is
//! rejected with exactly the typed error its mutation names, and no switch can
//! disable a check. Vectors are written by `conformance/exporter blocks` (valid) and
//! `conformance/negative-gen` (negative); nothing here is typed by hand.

#[path = "../../../conformance/negative-gen/src/vector.rs"]
mod vector;

use std::collections::BTreeMap;

use alloy_primitives::Bytes;
use vector::{VContext, Vector};
use xone_consensus::verify::{
    Body, BodyError, BodyVerifier, Context, HeaderError, HeaderVerifier, V1Context, V2Context,
};
use xone_primitives::Header;

fn header_of(b: &Bytes) -> Header {
    alloy_rlp::decode_exact(b).unwrap()
}

fn context_of(c: &VContext) -> Context {
    match c.consensus.as_str() {
        "v1" => Context::V1(Box::new(V1Context {
            masternodes: c.masternodes.clone(),
            parent_masternodes: c.parent_masternodes.clone(),
            signers: c.signers.clone(),
            checkpoint: c.checkpoint.as_ref().map(header_of),
            checkpoint_signers: c.checkpoint_signers.clone(),
        })),
        "v2" => Context::V2(V2Context {
            masternodes: c.masternodes.clone(),
            penalties: c.penalties.clone(),
            parent_masternodes: (!c.parent_masternodes.is_empty())
                .then(|| c.parent_masternodes.clone()),
            parent_epoch_switch: c
                .parent_epoch_switch
                .expect("V2 vector without parentEpochSwitch"),
        }),
        other => panic!("consensus {other}"),
    }
}

fn run_header(v: &Vector) -> Result<(), HeaderError> {
    let spec = vector::spec_of(v);
    HeaderVerifier::new(&spec).verify(
        &header_of(&v.header),
        &header_of(&v.parent),
        v.context.now,
        &context_of(&v.context),
    )
}

fn run_body(v: &Vector) -> Result<(), BodyError> {
    let spec = vector::spec_of(v);
    let body = Body {
        transactions: v.body.transactions.clone(),
        uncles: v.body.uncles.iter().map(header_of).collect(),
        withdrawals: v.body.withdrawals.clone(),
    };
    BodyVerifier::new(&spec).verify(&header_of(&v.header), &body, &v.receipts)
}

fn valid() -> Vec<Vector> {
    let mut all = vec![];
    for net in ["apothem", "net2040"] {
        all.extend(vector::load_file(
            &vector::vectors_dir().join(format!("valid/{net}.jsonl")),
        ));
    }
    all
}

fn negatives() -> Vec<Vector> {
    let dir = vector::vectors_dir().join("negative");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();
    files.iter().flat_map(|f| vector::load_file(f)).collect()
}

#[test]
fn every_valid_vector_is_accepted() {
    let all = valid();
    let mut classes: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut failures = vec![];
    for v in &all {
        *classes
            .entry((v.network.clone(), v.class.clone()))
            .or_default() += 1;
        if header_of(&v.header).hash() != v.hash {
            failures.push(format!(
                "{}: header rlp does not hash to the archive hash",
                v.id
            ));
        }
        if let Err(e) = run_header(v) {
            failures.push(format!("{}: header: {e:?}", v.id));
        }
        if let Err(e) = run_body(v) {
            failures.push(format!("{}: body: {e:?}", v.id));
        }
    }
    println!("valid vectors per class: {classes:#?}");
    assert!(
        failures.is_empty(),
        "{} of {} valid vectors rejected:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}

#[test]
fn every_negative_vector_is_rejected_with_its_error() {
    let all = negatives();
    let mut per_rule: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures = vec![];
    for v in &all {
        let m = v
            .mutation
            .as_ref()
            .expect("a negative vector names its mutation");
        let got = match m.stage.as_str() {
            "header" => run_header(v).err().map(|e| e.code()),
            "body" => run_body(v).err().map(|e| e.code()),
            s => panic!("stage {s}"),
        };
        *per_rule.entry(m.rule.clone()).or_default() += 1;
        if got.as_deref() != Some(m.expect.as_str()) {
            failures.push(format!(
                "{} [{}]: want {}, got {:?}",
                v.id, m.rule, m.expect, got
            ));
        }
    }
    println!("negative vectors per rule: {per_rule:#?}");
    assert!(
        failures.is_empty(),
        "{} of {} negative vectors wrong:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}

#[test]
fn the_corpus_has_the_required_shape() {
    let all = valid();
    let count = |net: &str| all.iter().filter(|v| v.network == net).count();
    assert!(
        count("apothem") >= 300 && count("net2040") >= 100,
        "{} {}",
        count("apothem"),
        count("net2040")
    );
    for class in [
        "v1_epoch",
        "v1",
        "v2_switch",
        "v2_epoch",
        "v2",
        "sign",
        "side",
    ] {
        assert!(
            all.iter().any(|v| v.class == class),
            "no vector of class {class}"
        );
    }
    assert!(all.iter().filter(|v| v.class == "v2_epoch").count() >= 20);
}

/// No Cargo feature, environment variable or boolean switch in the consensus or
/// execution sources may turn a check off.
#[test]
fn no_switch_can_disable_a_check() {
    const FORBIDDEN: [&str; 17] = [
        "cfg(feature",
        "cfg!(feature",
        "cfg_attr(feature",
        "env::var",
        "std::env",
        "option_env!",
        "skip_check",
        "skip_verif",
        "skip_valid",
        "no_verify",
        "approx",
        "lenient",
        "toleran",
        "bypass",
        "no_verify",
        "dont_check",
        "unchecked",
    ];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![];
    for krate in ["xone-consensus", "xone-evm"] {
        let src = root.join(krate).join("src");
        if src.is_dir() {
            walk(&src, &mut files);
        }
    }
    assert!(!files.is_empty());
    let mut hits = vec![];
    for f in &files {
        for (i, line) in std::fs::read_to_string(f).unwrap().lines().enumerate() {
            let code = line.split("//").next().unwrap().to_lowercase();
            for word in FORBIDDEN {
                if code.contains(word) {
                    hits.push(format!(
                        "{}:{}: {word}: {}",
                        f.display(),
                        i + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "possible check switches:\n{}",
        hits.join("\n")
    );
    // The crate manifests declare no feature either.
    for krate in ["xone-consensus", "xone-evm"] {
        let manifest = root.join(krate).join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            assert!(
                !text.contains("[features]"),
                "{} declares features",
                manifest.display()
            );
        }
    }
}

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}
