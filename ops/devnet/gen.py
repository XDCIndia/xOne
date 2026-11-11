#!/usr/bin/env python3
"""Makes the files of a private two-node devnet for the first local mint (docs/ops/first-mint-local.md).

    gen.py OUTDIR GETH_BIN

OUTDIR gets: `keystore.json` and `password` (a throwaway key, mode 0600), `genesis.json` (the pinned geth-XDC
devnet genesis with a short epoch and ONE masternode, the throwaway key, in the V1 checkpoint list and in the
candidate list of the 0x88 contract), and `forks.json` (the fork table of that chain for `xone --chainspec`).
Nothing here touches a public network or a real key. Needs `cast` (foundry) for the keystore and the storage slots,
and the geth-XDC binary for the genesis hash.
"""
import json, os, shutil, subprocess, sys, tempfile

out, geth = sys.argv[1], sys.argv[2]
repo = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
EPOCH, GAP, SWITCH, SWITCH_EPOCH = 20, 10, 40, 2   # V1 for 40 blocks, then V2; short so that a run crosses it
os.makedirs(out, exist_ok=True)

def run(*a):
    return subprocess.run(a, check=True, capture_output=True, text=True).stdout.strip()

# the throwaway key
pw = "throwaway-password"
ks_dir = os.path.join(out, "ks")
os.makedirs(ks_dir, exist_ok=True)
txt = run("cast", "wallet", "new", ks_dir, "xone-validator", "--unsafe-password", pw)
addr = [l.split()[-1] for l in txt.splitlines() if "Address" in l][0].lower().removeprefix("0x")
os.replace(os.path.join(ks_dir, "xone-validator"), os.path.join(out, "keystore.json"))
os.rmdir(ks_dir)
open(os.path.join(out, "password"), "w").write(pw + "\n")
for f in ("keystore.json", "password"):
    os.chmod(os.path.join(out, f), 0o600)

# the genesis
g = json.load(open(os.path.join(repo, "crates/xone-chainspec/genesis/devnet.json")))
x = g["config"]["XDPoS"]
x.update(epoch=EPOCH, rewardCheckpoint=EPOCH, gap=GAP)
x["v2"]["SwitchEpoch"], x["v2"]["switchBlock"] = SWITCH_EPOCH, SWITCH
x["v2"]["allConfigs"] = {"0": x["v2"]["config"]}  # geth patches in the devnet row of round 50000 itself
g["extraData"] = "0x" + "00" * 32 + addr + "00" * 65
v = g["alloc"]["0000000000000000000000000000000000000088"]["storage"]
def slot(n): return "0x" + n.to_bytes(32, "big").hex()
v[slot(8)] = slot(1)                                              # the candidate array has one entry
first = run("cast", "keccak", slot(8))                            # its first element
v[first] = "0x" + "00" * 12 + addr
owner = run("cast", "index", "address", "0x" + addr, "1")         # validatorsState[me]: owner, cap
v[owner] = "0x" + "00" * 12 + addr
v["0x" + (int(owner, 16) + 1).to_bytes(32, "big").hex()] = "0x" + (10_000_000 * 10**18).to_bytes(32, "big").hex()
json.dump(g, open(os.path.join(out, "genesis.json"), "w"), indent=1)

# the genesis hash and state root, from geth itself
d = tempfile.mkdtemp()
run(geth, "--datadir", d, "init", os.path.join(out, "genesis.json"))
b = run(geth, "--datadir", d, "--networkid", "5551", "--nodiscover", "--maxpeers", "0", "--port", "0",
        "--authrpc.port", "0", "--verbosity", "0", "console", "--exec", "var b=eth.getBlockByNumber(0); b.hash+' '+b.stateRoot")
hash_, root = b.strip().strip('"').split()
# the fork table: the devnet row with this chain's XDPoS numbers and genesis
t = json.load(open(os.path.join(repo, "crates/xone-chainspec/generated/forks.json")))
n = t["networks"]["devnet"]
n["xdpos"].update(epoch=EPOCH, rewardCheckpoint=EPOCH, gap=GAP)
n["xdpos"]["v2"]["switchBlock"], n["xdpos"]["v2"]["switchEpoch"] = str(SWITCH), SWITCH_EPOCH
n["genesis"] = {"hash": hash_, "stateRoot": root}
json.dump({"pin": t["pin"], "networks": {"private": n}}, open(os.path.join(out, "forks.json"), "w"), indent=1)
shutil.rmtree(d)
print(json.dumps({"address": "0x" + addr, "genesis": hash_, "stateRoot": root}))
