"""Tables for docs/pq/lab-results-*.md and docs/pq/bandwidth-model.md. Pure arithmetic.
Run as: python3 -I labs/pq/model.py results|tolerance|bandwidth RUN1.jsonl [RUN2.jsonl ...]
Inputs are the JSON lines written by `pq-lab`. Every assumption is a named constant below
and is repeated in docs/pq/bandwidth-model.md. Nothing here is a measurement of a network.
"""
import json
import statistics
import sys

# Assumptions (A-numbers match docs/pq/bandwidth-model.md)
SLOT_S = 2          # A2: chainspec period
EPOCH_BLOCKS = 900  # A2: chainspec epoch
TPS = (1000, 3000)  # A5: stated load
MASTERNODES = 108   # A1: V2 maximum in the chainspec
QC_SIGS = (8, 21, 73, 78, 108)  # A3: net2040 median, Apothem median, threshold at 108, Apothem max, all
VOTE_FIELDS = 64    # A6: non-signature bytes of one vote (assumed)
CRYPTO_BUDGET_MS = 150  # docs/perf.md section 6: cryptographic checks, p99 target
SCHEMES = [  # (column label, implementation, scheme)
    ("ECDSA", "k256", "ECDSA-secp256k1"),
    ("ML-DSA-44", "ml-dsa", "ML-DSA-44"),
    ("ML-DSA-65", "ml-dsa", "ML-DSA-65"),
    ("ML-DSA-87", "ml-dsa", "ML-DSA-87"),
    ("SLH-128s", "fips205", "SLH-DSA-SHAKE-128s"),
    ("SLH-128f", "fips205", "SLH-DSA-SHAKE-128f"),
    ("SLH-192f", "fips205", "SLH-DSA-SHAKE-192f"),
]
BLOCKS_DAY = 86400 // SLOT_S


def load(paths):
    runs = [[json.loads(l) for l in open(p)] for p in paths]
    key = lambda d: (d["impl"], d["scheme"], d["op"])
    out = {}
    for r in runs:
        for d in r:
            out.setdefault(key(d), []).append(d)
    return out


def sizes(data):
    d = {lab: data[(i, s, "sign")][0] for lab, i, s in SCHEMES}
    return {lab: (d[lab]["pk_bytes"], d[lab]["sig_bytes"]) for lab in d}


def p50_us(data, lab, op):
    i, s = next((i, s) for l, i, s in SCHEMES if l == lab)
    return statistics.median(r["p50_ns"] for r in data[(i, s, op)]) / 1000


def verify_ops(lab):  # the chain recovers ECDSA signers; a PQ scheme verifies. ECDSA is shown both ways
    return ("recover", "verify_valid") if lab == "ECDSA" else ("verify_valid",)


def fmt_bytes(n):
    for unit, div in (("TB", 1e12), ("GB", 1e9), ("MB", 1e6), ("kB", 1e3)):
        if abs(n) >= div:
            return f"{n / div:,.2f} {unit}"
    return f"{n:,.0f} B"


def vs_ecdsa(total, sig):
    return fmt_bytes(total) + ("" if sig == 65 else f" ({sig / 65:.1f}x)")


def row(*cells):
    return "| " + " | ".join(str(c) for c in cells) + " |"


def table(head, rows):
    return "\n".join([row(*head), row(*["---"] * len(head))] + [row(*r) for r in rows])


def results(data):
    ops = ["keygen", "sign", "verify_valid", "verify_invalid_msg", "verify_invalid_flip", "recover"]
    order = [(i, s) for _, i, s in SCHEMES] + [("fips204", f"ML-DSA-{n}") for n in (44, 65, 87)]
    rows = []
    for imp, sch in order:
        for op in ops:
            rs = data.get((imp, sch, op))
            if not rs:
                continue
            p50 = [r["p50_ns"] / 1000 for r in rs]
            p99 = [r["p99_ns"] / 1000 for r in rs]
            mn = [r["min_ns"] / 1000 for r in rs]
            sp = lambda v: 100 * (max(v) - min(v)) / statistics.median(v)
            rows.append((imp, sch, op, "/".join(str(r["n"]) for r in rs), f"{statistics.median(p50):,.1f}",
                         f"{sp(p50):.0f}%", f"{statistics.median(p99):,.1f}", f"{sp(p99):.0f}%",
                         f"{statistics.median(mn):,.1f}", "/".join(f"{x:,.1f}" for x in p50), "/".join(f"{x:,.1f}" for x in p99)))
    return table(["Library", "Scheme", "Operation", "n per run", "p50 us (median of runs)", "p50 spread",
                  "p99 us (median of runs)", "p99 spread", "min us (median of runs)", "p50 per run", "p99 per run"], rows)


def tolerance(data, p50_tol=10, p99_tol=20):
    """docs/perf.md section 4: p50 within 10 percent and p99 within 20 percent across the runs."""
    sp = lambda v: 100 * (max(v) - min(v)) / statistics.median(v)
    bad, total = [], 0
    for (imp, sch, op), rs in sorted(data.items()):
        total += 1
        a, b = sp([r["p50_ns"] for r in rs]), sp([r["p99_ns"] for r in rs])
        if a > p50_tol or b > p99_tol:
            bad.append(f"{imp}/{sch}/{op} (p50 {a:.0f}%, p99 {b:.0f}%)")
    return f"{total - len(bad)} of {total} rows within tolerance; {len(bad)} over:\n\n" + "\n".join(f"- {x}" for x in bad)


def bandwidth(data):
    sz = sizes(data)
    labs = [l for l, _, _ in SCHEMES]
    out = []
    out.append("### Table 1. Sizes from the lab (bytes)\n")
    out.append(table(["Item"] + labs, [["Public key"] + [f"{sz[l][0]:,}" for l in labs],
                                       ["Signature"] + [f"{sz[l][1]:,}" for l in labs],
                                       ["Signature, ratio to ECDSA 65 B"] + [f"{sz[l][1] / 65:.1f}x" for l in labs]]))
    out.append("\n### Table 2. Transaction signatures (one per transaction, no aggregation)\n")
    rows = []
    for tps in TPS:
        per_block = tps * SLOT_S
        for variant, add_pk in (("signature only", False), ("signature + public key", True)):
            cells = []
            for l in labs:
                pk, sg = sz[l]
                b = sg + (pk if add_pk and l != "ECDSA" else 0)
                cells.append(f"{fmt_bytes(tps * b)}/s; {fmt_bytes(per_block * b)}/block; {fmt_bytes(tps * b * 86400)}/day")
            rows.append([f"{tps:,} TPS ({per_block:,} tx per block), {variant}"] + cells)
    out.append(table(["Load and variant"] + labs, rows))
    out.append("\nECDSA carries no public key: the sender is recovered from the signature.")
    out.append(f"\n### Table 3. Votes: {MASTERNODES} masternodes, one vote each per {SLOT_S} s round, no aggregation\n")
    rows = []
    for m in (8, 78, MASTERNODES):
        cells = []
        for l in labs:
            sg = sz[l][1]
            per_round = m * (sg + VOTE_FIELDS)
            cells.append(f"{fmt_bytes(per_round)}/round; {fmt_bytes(per_round / SLOT_S)}/s")
        rows.append([f"{m} voters"] + cells)
    out.append(table(["Voters per round"] + labs, rows))
    out.append("\n### Table 4. Parent QC inside every header: signature bytes per header and growth\n")
    rows = [[f"{q} sigs per header"] + [fmt_bytes(q * sz[l][1]) for l in labs] for q in QC_SIGS]
    out.append(table(["QC signatures"] + labs, rows))
    out.append(f"\nGrowth per epoch ({EPOCH_BLOCKS} blocks, {EPOCH_BLOCKS * SLOT_S // 60} min):\n")
    rows = [[f"{q} sigs"] + [vs_ecdsa(EPOCH_BLOCKS * q * sz[l][1], sz[l][1]) for l in labs] for q in QC_SIGS]
    out.append(table(["QC signatures"] + labs, rows))
    out.append(f"\nGrowth per day ({BLOCKS_DAY:,} blocks) and per year (x365):\n")
    rows = [[f"{q} sigs"] + [vs_ecdsa(BLOCKS_DAY * q * sz[l][1], sz[l][1]) + f"; {fmt_bytes(365 * BLOCKS_DAY * q * sz[l][1])}/yr" for l in labs] for q in QC_SIGS]
    out.append(table(["QC signatures"] + labs, rows))
    out.append("\nThe factor in brackets is the ratio to the ECDSA column in the same row (equal to the signature-size ratio).")
    out.append("\nHybrid (ECDSA and the scheme, both required): signature bytes per header at q signatures are q x (65 + S):\n")
    rows = [[f"{q} sigs"] + [fmt_bytes(q * (65 + sz[l][1])) if l != "ECDSA" else "n/a" for l in labs] for q in (21, 78)]
    out.append(table(["QC signatures"] + labs, rows))
    out.append(f"\nValidator public-key set per epoch snapshot ({MASTERNODES} keys, if a registry delivers them once per epoch): "
               + "; ".join(f"{l} {fmt_bytes(MASTERNODES * sz[l][0])}" for l in labs if l != "ECDSA") + ".")
    out.append("\n### Table 5. Verification CPU per block at the measured p50 (single core, no batching, no parallelism)\n")
    rows = []
    for tps in TPS:
        for l, op in [(l, op) for l in labs for op in verify_ops(l)]:
            t = p50_us(data, l, op)
            tx = tps * SLOT_S * t / 1000
            qc = 78 * t / 1000
            votes = MASTERNODES * t / 1000
            rows.append([f"{tps:,}", l, op, f"{t:,.1f}", f"{tx:,.0f}", f"{qc:,.2f}", f"{votes:,.2f}",
                         f"{tx + qc + votes:,.0f}", f"{100 * (tx + qc + votes) / (SLOT_S * 1000):.1f}%",
                         f"{(tx + qc + votes) / CRYPTO_BUDGET_MS:.1f}"])
    out.append(table(["TPS", "Scheme", "Operation", "p50 us", "Txs ms/block", "QC (78) ms/block", "Votes (108) ms/round",
                      "Total ms", "Share of one core over a 2 s slot", "Cores to fit 150 ms (arithmetic)"], rows))
    return "\n".join(out)


if __name__ == "__main__":
    if len(sys.argv) < 3 or sys.argv[1] not in ("results", "bandwidth", "tolerance"):
        sys.exit(__doc__)
    d = load(sys.argv[2:])
    print({"results": results, "bandwidth": bandwidth, "tolerance": tolerance}[sys.argv[1]](d))
