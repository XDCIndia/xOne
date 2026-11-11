import sys, json, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import build_reward as B
V2F = [83599011, 83599875, 57000214, 62000543, 68000677, 74000694, 79000702, 83000425]
TIER = [83600502, 83601087, 83601986, 84731521, 84732420, 84733320, 84734201, 84735099, 84000672, 84400386, 84900072, 85100879]
EDGE = [56829393, 56830293]
V1 = [3601800, 8100000, 15300000, 23400000, 30600000, 40500000, 47700000, 53100000, 56826900, 56827800, 56828700]
jobs = [(n, "tiered") for n in TIER] + [(n, "v2") for n in V2F] + [(n, "v2edge") for n in EDGE] + [(n, "v1") for n in V1]
os.makedirs("raw", exist_ok=True)
for n, k in jobs:
    f = f"raw/{n}.json"
    if os.path.exists(f):
        continue
    try:
        r = B.build(n, k)
        json.dump(r, open(f, "w"))
        print("ok", n, k, flush=True)
    except Exception as e:
        print("FAIL", n, k, repr(e), flush=True)
