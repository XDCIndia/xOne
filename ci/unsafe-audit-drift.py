#!/usr/bin/env python3
"""Fail when the committed unsafe/FFI table lists other packages, versions, unsafe counts or FFI flags
than a fresh `ci/unsafe-audit.sh` run. The reason and owner-review columns are not compared.
  python3 -I ci/unsafe-audit-drift.py FRESH.md COMMITTED.md
"""
import sys

def rows(path):
    out = {}
    for line in open(path, encoding="utf-8"):
        c = [x.strip() for x in line.split("|")]
        if len(c) > 6 and c[1] not in ("crate", "---") and not c[1].startswith("-"):
            out[(c[1], c[2])] = (c[3], c[4])
    return out

fresh, kept = rows(sys.argv[1]), rows(sys.argv[2])
bad = [f"new or changed: {k} {v}" for k, v in fresh.items() if kept.get(k) != v]
bad += [f"gone: {k}" for k in kept if k not in fresh]
for b in bad:
    print("UNSAFE AUDIT DRIFT", b, file=sys.stderr)
if bad:
    print("regenerate: ci/unsafe-audit.sh > docs/assurance/unsafe-ffi-audit-2026-11-11.md (keep owner-review marks)", file=sys.stderr)
    sys.exit(1)
print(f"unsafe-audit: {len(fresh)} rows match")
