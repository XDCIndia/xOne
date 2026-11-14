//! The shipped Grafana dashboard and Prometheus alert rules (#184) name only metric
//! families the node exports: every `xone_*` name in `ops/grafana/xone-overview.json` and
//! `ops/prometheus/alerts.yml` must have a `# TYPE` line in [`encode`]'s output.

use std::collections::BTreeSet;

use xone_node::observe::metrics::{encode, QueueStat, Snapshot};

fn exported() -> BTreeSet<String> {
    let s = Snapshot {
        head_number: Some(1),
        rpc_in_flight: vec![("c1", 0)],
        queues: vec![QueueStat {
            id: "Q1",
            ..QueueStat::default()
        }],
        ..Snapshot::default()
    };
    encode(&s)
        .lines()
        .filter_map(|l| l.strip_prefix("# TYPE "))
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

fn names_in(text: &str) -> BTreeSet<String> {
    let b = text.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while let Some(off) = text.get(i..).and_then(|t| t.find("xone_")) {
        let start = i + off;
        let boundary = start == 0
            || !b
                .get(start - 1)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
        let mut end = start;
        while b
            .get(end)
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
        {
            end += 1;
        }
        if boundary {
            if let Some(n) = text.get(start..end) {
                out.insert(n.to_owned());
            }
        }
        i = end.max(start + 1);
    }
    out
}

fn read(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn dashboard_and_alerts_name_only_exported_metrics() {
    let have = exported();
    assert!(have.contains("xone_head_block_age_seconds"), "{have:?}");
    for rel in [
        "ops/grafana/xone-overview.json",
        "ops/prometheus/alerts.yml",
    ] {
        let used = names_in(&read(rel));
        assert!(!used.is_empty(), "{rel} names no xone_ metric");
        let missing: Vec<_> = used.difference(&have).collect();
        assert!(
            missing.is_empty(),
            "{rel} names unknown metrics {missing:?}"
        );
    }
}

#[test]
fn dashboard_is_valid_json_with_panels() {
    let v: serde_json::Value = serde_json::from_str(&read("ops/grafana/xone-overview.json"))
        .unwrap_or_else(|e| panic!("dashboard JSON: {e}"));
    let panels = v
        .get("panels")
        .and_then(|p| p.as_array())
        .map_or(0, Vec::len);
    assert!(panels >= 10, "dashboard has {panels} panels");
    assert_eq!(v.get("uid").and_then(|u| u.as_str()), Some("xone-overview"));
}

#[test]
fn alerts_cover_head_age_peers_and_missed_turns() {
    let a = read("ops/prometheus/alerts.yml");
    for m in [
        "xone_head_block_age_seconds",
        "xone_peers",
        "xone_missed_eligible_turns_total",
    ] {
        assert!(names_in(&a).contains(m), "alerts.yml lacks {m}");
    }
}
