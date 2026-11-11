import sys, os, json, glob, subprocess
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
network = sys.argv[1]
rawdir = sys.argv[2]
out = sys.argv[3]
forks = sys.argv[4] if len(sys.argv) > 4 else ""
GETH = os.environ["GETH_SRC"]  # geth-XDC checkout at crates/xone-consensus/oracle/reward/PIN
ORACLE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../../../crates/xone-consensus/oracle/reward/run.sh")
raws = sorted((json.load(open(f)) for f in glob.glob(rawdir + "/*.json")), key=lambda r: r["number"])
cases, recs = [], []
for r in raws:
    nodes = r["nodes"]
    idx = {a: i for i, a in enumerate(nodes)}
    mn = set(r["masternodes"]); pr = set(r["protectors"]); ob = set(r["observers"])
    kind = r["kind"]
    tiered = kind == "tiered"
    signs, total = {}, 0
    t = {"masternode": {}, "protector": {}, "observer": {}}
    slots = 0
    for k, senders in r["signed"].items():
        slots += 1
        s = set(senders)
        if tiered:
            for name, members in (("masternode", mn), ("protector", pr), ("observer", ob)):
                for a in s & members:
                    t[name][a] = t[name].get(a, 0) + 1
        else:
            for a in s & mn:
                signs[a] = signs.get(a, 0) + 1
                total += 1
    voters = r.get("voters", {})
    holders = {a: {"owner": o, "voters": voters.get(a, [])} for a, o in zip(nodes, r["owners"])}
    case = {"id": f"{network}-{r['number']}", "network": network, "rule": "tiered" if tiered else "v2", "number": r["number"], "round": r["round"],
            "holders": holders}
    if tiered:
        case.update({"slots": slots, "masternode": t["masternode"], "protector": t["protector"], "observer": t["observer"]})
    else:
        case.update({"total": total, "signs": signs})
    cases.append(case)
    recs.append(r)
cf = os.path.join(rawdir, "cases.json")
json.dump(cases, open(cf, "w"))
env = dict(os.environ, GETH_SRC=GETH, PATH="/usr/local/go/bin:" + os.environ["PATH"])
p = subprocess.run([ORACLE, cf] + ([forks] if forks else []), capture_output=True, env=env)
if p.returncode:
    raise SystemExit(p.stderr.decode())
oracle = {o["id"]: {a.lower(): v for a, v in o["rewards"].items()} for o in json.loads(p.stdout)}
vecs = []
problems = []
for r, c in zip(recs, cases):
    E = oracle[c["id"]]
    nodes = r["nodes"]; idx = {a: i for i, a in enumerate(nodes)}
    delta = r.get("delta", {}); fee = r.get("fee", 0)
    fo = r.get("fee_owner")
    if fee > 0 and fo is None:
        cand = [a for a in delta if delta[a] - int(E.get(a, "0")) == fee]
        fo = cand[0] if len(cand) == 1 else None
    archive = {a: str(delta[a] - (fee if a == fo else 0)) for a in delta}
    signed = [[int(k), r["window_hash"][k], [idx[a] for a in senders]] for k, senders in r["signed"].items()]
    vecs.append({"id": c["id"], "network": network, "kind": r["kind"], "number": r["number"], "round": r["round"], "start": r["start"], "end": r["end"],
                 "parent_root": r["parent_root"], "nodes": nodes, "owners": r["owners"], "masternodes": [idx[a] for a in r["masternodes"]], "pool": r["pool"],
                 "signed": signed, "fee": {"total": str(fee), "owner": fo}, "archive": archive, "oracle": E, "voters": r.get("voters", {})})
    bad = {a: (archive[a], E.get(a, "0")) for a in archive if archive[a] != E.get(a, "0")}
    if bad:
        problems.append((r["number"], len(bad), list(bad.items())[:2]))
json.dump(vecs, open(out, "w"), separators=(",", ":"))
print("vectors", len(vecs), "archive != oracle:", problems)
