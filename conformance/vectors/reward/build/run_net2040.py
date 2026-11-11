import sys, json, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import build_reward as B
# epoch switches of net2040 around tipFixedRewardBlock = 16,200 (flat before, tiered fixed from it; no proportional row)
jobs = [(13499, "v2"), (14399, "v2"), (15299, "v2"), (16199, "v2"), (17099, "tiered"), (17999, "tiered"), (18823, "tiered")]
os.makedirs("raw_net2040", exist_ok=True)
for n, k in jobs:
    f = f"raw_net2040/{n}.json"
    if os.path.exists(f):
        continue
    try:
        json.dump(B.build(n, k), open(f, "w"))
        print("ok", n, k, flush=True)
    except Exception as e:
        print("FAIL", n, k, repr(e), flush=True)
