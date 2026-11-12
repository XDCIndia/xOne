#!/usr/bin/env python3
"""Makes the chain of the Gate D rehearsal (docs/ops/gate-d-rehearsal.md): N validators, each with its own throwaway key.

    gen-gate-d.py OUTDIR GETH_BIN [N]

OUTDIR gets `v0` .. `v{N-1}` (each: `keystore.json`, `password`, `address`; mode 0600), `genesis.json` and
`chain.json` (addresses in masternode order, genesis hash). The genesis is the pinned devnet genesis (its system
contracts) with a private chain id, `epoch 30, gap 10`, V2 switch at block 30, 2 s blocks, every fork at a low
height in net2040's order, and the N keys as the V1 masternodes and the only candidates of the 0x88 contract
(owner and voter of themselves). Both clients read the forks from the genesis `config` block (xone: no
`--chainspec`). Nothing here touches a public network or a real key. Needs `cast` (foundry).
"""
import json, os, shutil, subprocess, sys, tempfile, time

out, geth = sys.argv[1], sys.argv[2]
n = int(sys.argv[3]) if len(sys.argv) > 3 else 4
repo = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CHAIN_ID, EPOCH, GAP, SWITCH = 4747, 30, 10, 30
CAP = 10_000_000 * 10**18
os.makedirs(out, exist_ok=True)


def run(*a):
    return subprocess.run(a, check=True, capture_output=True, text=True).stdout.strip()


def slot(x):
    return "0x" + x.to_bytes(32, "big").hex()


def word(addr, flag=0):  # an address, with a bool packed after it (`owner, isCandidate`)
    return "0x" + "00" * 11 + f"{flag:02x}" + addr


pw = "throwaway-password"
addrs = []
for i in range(n):
    d = os.path.join(out, f"v{i}")
    os.makedirs(d, exist_ok=True)
    txt = run("cast", "wallet", "new", d, "key", "--unsafe-password", pw)
    a = [l.split()[-1] for l in txt.splitlines() if "Address" in l][0].lower().removeprefix("0x")
    ks = json.load(open(os.path.join(d, "key")))
    ks["address"] = a  # geth indexes a keystore by this field; cast leaves it out
    json.dump(ks, open(os.path.join(d, "keystore.json"), "w"))
    os.remove(os.path.join(d, "key"))
    open(os.path.join(d, "password"), "w").write(pw + "\n")
    open(os.path.join(d, "address"), "w").write("0x" + a + "\n")
    for f in ("keystore.json", "password"):
        os.chmod(os.path.join(d, f), 0o600)
    addrs.append(a)

g = json.load(open(os.path.join(repo, "crates/xone-chainspec/genesis/devnet.json")))
c = g["config"]
# net2040's fork order (crates/xone-chainspec/generated/forks.json), compressed below block 160
forks = dict(homesteadBlock=1, tip2019Block=1, eip150Block=2, eip155Block=3, eip158Block=3, byzantiumBlock=4,
             constantinopleBlock=4, petersburgBlock=4, istanbulBlock=4, tipSigningBlock=5, tipRandomizeBlock=6,
             tipIncreaseMasternodesBlock=7, denylistBlock=8, tipNoHalvingMNRewardBlock=8, tipXDCXBlock=8,
             tipXDCXLendingBlock=8, tipXDCXCancellationFeeBlock=8, tipTRC21FeeBlock=8, berlinBlock=9, londonBlock=9,
             mergeBlock=9, shanghaiBlock=9, gas50xBlock=12, tipXDCXMinerDisableBlock=12,
             tipXDCXReceiverDisableBlock=13, gas2500xBlock=14, eip1559Block=14, cancunBlock=16,
             dynamicGasLimitBlock=40, pragueBlock=45, osakaBlock=50, amsterdamBlock=55,
             tipUpgradeRewardBlock=150, tipUpgradePenaltyBlock=150, tipEpochHalvingBlock=999999999)
for k in [k for k in c if k.endswith("Block")]:
    del c[k]
c.update(chainId=CHAIN_ID, **forks)
x = c["XDPoS"]
x.update(epoch=EPOCH, rewardCheckpoint=EPOCH, gap=GAP, maxMasternodesV2=108)
row = dict(x["v2"]["config"], timeoutPeriod=10, minePeriod=2, masternodeReward=63.4196, protectorReward=50.7357,
           observerReward=25.3678, maxProtectorNodes=432, maxObserverNodes=99999, minimumMinerBlockPerEpoch=1,
           limitPenaltyEpoch=2, minimumSigningTx=1, expTimeoutConfig={"base": 1.0, "maxExponent": 0})
x["v2"].pop("SwitchEpoch", None)
x["v2"].update(switchBlock=SWITCH, switchEpoch=SWITCH // EPOCH, config=row, allConfigs={"0": row})

# geth's validator does not seal while its head is older than its at-tip window (`eth/sync_xdc.go:740-763`, about
# 30 s): a genesis stamped a few seconds ahead of the start lets the in-turn geth seal block 1
g["timestamp"] = hex(int(time.time()) + 10)
order = sorted(addrs)  # the V1 masternode list of the genesis checkpoint, ascending
g["extraData"] = "0x" + "00" * 32 + "".join(order) + "00" * 65
v = g["alloc"]["0000000000000000000000000000000000000088"]["storage"]
v[slot(8)] = v[slot(9)] = slot(len(order))  # candidates.length, candidateCount
first = int(run("cast", "keccak", slot(8)), 16)
for i, a in enumerate(order):
    v[slot(first + i)] = "0x" + "00" * 12 + a
    state = int(run("cast", "index", "address", "0x" + a, "1"), 16)  # validatorsState[a]
    v[slot(state)] = word(a, 1)  # owner = a, isCandidate
    v[slot(state + 1)] = slot(CAP)
    v[run("cast", "index", "address", "0x" + a, str(state + 2))] = slot(CAP)  # its own voter cap
    voters = run("cast", "index", "address", "0x" + a, "2")  # voters[a] = [a]
    v[voters] = slot(1)
    v[run("cast", "keccak", voters)] = "0x" + "00" * 12 + a
json.dump(g, open(os.path.join(out, "genesis.json"), "w"), indent=1)

d = tempfile.mkdtemp()
run(geth, "--datadir", d, "init", os.path.join(out, "genesis.json"))
b = run(geth, "--datadir", d, "--networkid", str(CHAIN_ID), "--nodiscover", "--maxpeers", "0", "--port", "0",
        "--authrpc.port", "0", "--verbosity", "0", "console", "--exec",
        "var b=eth.getBlockByNumber(0); b.hash+' '+b.stateRoot")
shutil.rmtree(d)
hash_, root = b.strip().strip('"').split()
info = {"chainId": CHAIN_ID, "genesis": hash_, "stateRoot": root, "masternodes": ["0x" + a for a in order],
        "keys": {f"v{i}": "0x" + a for i, a in enumerate(addrs)}}
json.dump(info, open(os.path.join(out, "chain.json"), "w"), indent=1)
print(json.dumps(info))
