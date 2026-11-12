#!/usr/bin/env python3
"""Prints the newest safety record of an xone validator datadir (ADR-010 section 3, the two-slot file of section 4).

    record.py DATADIR

Read-only. The frame checksum (keccak256) is not checked here: the node checks it at start; this picks the slot
with the higher store sequence whose record carries the magic.
"""
import json, struct, sys

raw = open(f"{sys.argv[1]}/xone/safety/safety.rec", "rb").read()
best = None
for slot in (raw[:1024], raw[1024:2048]):
    seq, il = struct.unpack_from("<QH", slot)
    (rl,) = struct.unpack_from("<H", slot, 10 + il)
    rec = slot[12 + il:12 + il + rl]
    if rec[:4] == b"XSR1" and (best is None or seq > best[0]):
        best = (seq, rec)
seq, r = best
o = 26
rseq, hvr = struct.unpack_from("<QQ", r, o)
o += 16
names = ("vote", "timeout", "propose", "seal_v1")
signed = {}
for n in names:
    (rnd,) = struct.unpack_from("<Q", r, o)
    signed[n] = {"round": rnd, "digest": r[o + 8:o + 40].hex()[:16]}
    o += 40
lock_flag = r[o]
o += 1
def ref(at):
    h = r[at:at + 32].hex()[:16]
    rnd, num = struct.unpack_from("<QQ", r, at + 32)
    return {"round": rnd, "number": num, "hash": h}
out = {"store_seq": seq, "seq": rseq, "highest_voted_round": hvr, "signed": signed,
       "lock": ref(o) if lock_flag else None, "highest_qc": ref(o + 48)}
(out["highest_tc"],) = struct.unpack_from("<Q", r, o + 96)
print(json.dumps(out))
