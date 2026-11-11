"""Layering check (plan #1 Layout, R3). Reads `cargo metadata --no-deps` on stdin.

Usage: cargo metadata --format-version 1 --no-deps | python3 -I ci/layering.py
Exit 1 and print each violation. Standard library only.
"""
import json
import os
import sys

# Dependencies point to a strictly lower rank only ("upward in this list only").
RANK = {
    "xone-primitives": 0,
    "xone-chainspec": 1,
    "xone-state": 2,
    "xone-evm": 2,
    "xone-signer": 2,  # below xone-consensus: its safety unit calls it (ADR-010 section 10)
    "xone-consensus": 3,
    "xone-db": 4,
    "xone-net": 5,
    "xone-snap": 5,
    "xone-sync": 5,
    "xone-builder": 6,
    "xone-rpc": 6,
    "xone-node": 7,
    "xone": 8,  # bin/xone
}
# Crates that only a named set of crates may depend on (ADR-010 section 10, issue #40): the V2
# safety unit lives in xone-consensus, and xone-node holds the node key. Checked for every
# dependency kind, so a dev-dependency from another crate fails too. A re-export that hands the
# signer to a third crate needs its own dependency edge, which this rule rejects.
RESTRICTED = {"xone-signer": {"xone-consensus", "xone-node"}}
# Test-only trees: may depend on anything, nothing may depend on them.
TEST_ONLY_DIRS = ("sim", "spikes", "labs", "conformance")

# Wrong-direction edges that exist today, each with the reason and the way out.
# An edge listed here is allowed only as a normal dependency and is printed on every run.
EXCEPTIONS = {
    ("xone-consensus", "xone-db"): "V2 safety unit uses xone_db::{Durability, Cause}; "
    "plan #1 Layout puts db after consensus. Remove by moving those two types "
    "(and SafetyStore) to xone-primitives or xone-consensus, then delete this line.",
    ("xone-net", "xone-node"): "xone-net scores peers and bounds its queues with xone_node::{PeerBudget, "
    "BoundedQueue, Overflow} (ADR-007), which live in xone-node. Remove by moving those "
    "std-only primitives to a lower crate, then delete this line.",
}


def classify(pkg, root):
    rel = os.path.relpath(os.path.dirname(pkg["manifest_path"]), root).replace(os.sep, "/")
    top = rel.split("/")[0]
    if top in TEST_ONLY_DIRS:
        return "test", rel
    if rel == "bin/xone" and pkg["name"] == "xone":
        return "layer", rel
    if top == "crates" and rel == "crates/" + pkg["name"] and pkg["name"] in RANK:
        return "layer", rel
    return "unknown", rel


def check(meta):
    root = meta["workspace_root"]
    members = {p["id"]: p for p in meta["packages"] if p["id"] in set(meta["workspace_members"])}
    by_name = {p["name"]: p for p in members.values()}
    kind = {}
    errs, notes = [], []
    for p in members.values():
        k, rel = classify(p, root)
        kind[p["name"]] = k
        if k == "unknown":
            errs.append(f"{p['name']} ({rel}): not in the crate allow-list")
    for p in members.values():
        for d in p["dependencies"]:
            if d["name"] not in by_name or kind[p["name"]] == "unknown":
                continue
            src, dst = p["name"], d["name"]
            dk = d["kind"] or "normal"
            if kind[dst] == "test" and kind[src] != "test":
                errs.append(f"{src} -> {dst} ({dk}): nothing may depend on a test-only crate")
            elif dst in RESTRICTED and kind[src] == "layer" and src not in RESTRICTED[dst]:
                errs.append(
                    f"{src} -> {dst} ({dk}): only {sorted(RESTRICTED[dst])} may depend on {dst}"
                )
            elif kind[src] == "layer" and kind[dst] == "layer" and RANK[src] <= RANK[dst]:
                if (src, dst) in EXCEPTIONS and dk == "normal" and not os.environ.get("LAYERING_NO_EXCEPTIONS"):
                    notes.append(f"exception {src} -> {dst}: {EXCEPTIONS[(src, dst)]}")
                else:
                    errs.append(
                        f"{src} (rank {RANK[src]}) -> {dst} (rank {RANK[dst]}) ({dk}): "
                        "wrong direction, a crate may only depend on a lower rank"
                    )
    return errs, notes


if __name__ == "__main__":
    errs, notes = check(json.load(sys.stdin))
    for n in notes:
        print("note:", n)
    for e in errs:
        print("LAYERING:", e)
    sys.exit(1 if errs else 0)
