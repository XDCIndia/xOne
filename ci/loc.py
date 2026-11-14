#!/usr/bin/env python3
"""Per-crate lines-of-code and dependency report with ceilings (issue #201).

  ci/loc.py                      table of lines and normal-dependency count per crate
  ci/loc.py --base REF           add before/after/delta columns against REF
  ci/loc.py --budget FILE        fail when a crate named in FILE is above its ceiling
  ci/loc.py --root DIR           count another tree (default: the repo)
Lines = non-blank, non-comment Rust lines under crates/*/src, without `#[cfg(test)]` items,
`tests.rs` files and generated/ directories. # ponytail: brace counting ignores braces in strings.
"""
import json, os, subprocess, sys, tempfile, tomllib

def count(path):
    n, skip = 0, 0
    pending = False
    for line in open(path, encoding="utf-8", errors="replace"):
        s = line.strip()
        if skip:
            skip += line.count("{") - line.count("}")
            skip = max(skip, 0)
            continue
        if pending:
            if s.startswith("#["):
                continue
            pending = False
            skip = max(line.count("{") - line.count("}"), 0)
            continue
        if s.startswith("#[cfg(test)]"):
            pending = True
            continue
        if s and not s.startswith("//"):
            n += 1
    return n

def lines(root):
    out = {}
    for crate in sorted(os.listdir(os.path.join(root, "crates"))):
        total = 0
        for d, dirs, files in os.walk(os.path.join(root, "crates", crate, "src")):
            dirs[:] = [x for x in dirs if x not in ("generated", "tests")]
            for f in files:
                if f.endswith(".rs") and f != "tests.rs":
                    total += count(os.path.join(d, f))
        out[crate] = total
    return out

def deps(root):
    m = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--offline"], cwd=root,
        stderr=subprocess.DEVNULL))
    return {p["name"]: len({d["name"] for d in p["dependencies"] if d["kind"] is None})
            for p in m["packages"] if "/crates/" in p["manifest_path"]}

def snapshot(root):
    return lines(root), deps(root)

def main(argv):
    root, base, budget = os.getcwd(), None, None
    while argv:
        a = argv.pop(0)
        if a == "--root": root = argv.pop(0)
        elif a == "--base": base = argv.pop(0)
        elif a == "--budget": budget = argv.pop(0)
        else: sys.exit(__doc__)
    cl, cd = snapshot(root)
    bl = bd = None
    if base:
        with tempfile.TemporaryDirectory() as t:
            tar = subprocess.Popen(["git", "-C", root, "archive", base], stdout=subprocess.PIPE)
            subprocess.check_call(["tar", "-x", "-C", t], stdin=tar.stdout)
            bl, bd = snapshot(t)
    print("| crate | lines | deps |" + (" lines before | delta | deps before | delta |" if base else ""))
    print("| --- | ---: | ---: |" + (" ---: | ---: | ---: | ---: |" if base else ""))
    for c in cl:
        row = f"| {c} | {cl[c]} | {cd.get(c, 0)} |"
        if base:
            l0, d0 = bl.get(c, 0), bd.get(c, 0)
            row += f" {l0} | {cl[c]-l0:+d} | {d0} | {cd.get(c,0)-d0:+d} |"
        print(row)
    if budget:
        ceil = tomllib.load(open(budget, "rb"))["ceiling"]
        bad = [f"{c}: {cl.get(c, 0)} lines > ceiling {v}" for c, v in ceil.items() if cl.get(c, 0) > v]
        for b in bad: print("OVER BUDGET", b, file=sys.stderr)
        if bad: sys.exit(1)
        print("loc-budget: PASS")

main(sys.argv[1:])
