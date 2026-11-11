"""Summarise per-block latencies. Run as: python3 -I summarise.py FILE.csv [COLUMN]
CSV has a header; COLUMN names the latency column (default latency_ms), values in ms.
Prints JSON: n, p50, p95, p99, p99.9, max. Percentile = nearest rank (no interpolation),
so p99.9 over fewer than 1000 samples equals max; the output carries n so that is visible.
"""
import csv, json, sys


def pct(sorted_vals, permille):
    # integer arithmetic: float 99.9/100*n can round up past the true rank
    return sorted_vals[max(0, -(-permille * len(sorted_vals) // 1000) - 1)]


def summarise(vals):
    v = sorted(vals)
    if not v:
        raise ValueError("no samples")
    return {"n": len(v), "p50": pct(v, 500), "p95": pct(v, 950), "p99": pct(v, 990), "p99.9": pct(v, 999), "max": v[-1]}


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    col = sys.argv[2] if len(sys.argv) > 2 else "latency_ms"
    with open(sys.argv[1], newline="") as f:
        vals = [float(r[col]) for r in csv.DictReader(f)]
    print(json.dumps(summarise(vals)))
