import sys,time,json; sys.path.insert(0,'.')
import arch
from common import P, NETCFG
lo_all, hi_all = int(sys.argv[1]), int(sys.argv[2])
sw=[]
for lo in range(lo_all, hi_all, 200_000):
    r=arch.call(P,"XDPoS_getEpochNumbersBetween",[hex(max(lo,int(NETCFG["xdpos"]["v2"]["switchBlock"])+1)),hex(min(lo+200_000,hi_all))])
    sw+=r
sw=sorted(set(sw))
print("switches",len(sw),flush=True)
out={}
for s in range(0,len(sw),20):
    ch=sw[s:s+20]
    res=arch.batch(P,[("eth_getBlockByNumber",[hex(n),False]) for n in ch])
    for n,b in zip(ch,res):
        p=b["penalties"]; v=b["validators"]
        out[n]={"pen":[ "0x"+p[2:][i:i+40] for i in range(0,len(p)-2,40)],"val":["0x"+v[2:][i:i+40] for i in range(0,len(v)-2,40)],"hash":b["hash"],"root":b["stateRoot"],"miner":b["miner"],"parent":b["parentHash"]}
json.dump(out,open(f"sw_{lo_all}_{hi_all}.json","w"))
print("done",len(out))
