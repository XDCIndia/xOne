import sys, json, random; sys.path.insert(0,'.')
import arch
from build_reward import v1_masternodes
rnd = random.Random(11)
cands = sorted(rnd.sample(range(3_600_000 // 900, 56_820_000 // 900), 140))
res = []
for q in cands:
    N = q * 900
    try:
        a = arch.call(21105, "XDPoS_getSnapshot", [hex(N - 1)])["signers"]
        h = arch.call(21105, "eth_getBlockByNumber", [hex(N), False])
    except Exception as e:
        continue
    ex = v1_masternodes(h)
    res.append((N, len(a), len(ex), len(set(a) - set(ex)), len(set(ex) - set(a)), h["penalties"]))
json.dump(res, open("v1scan.json", "w"))
print(sum(1 for r in res if r[3] > 0), len(res))
