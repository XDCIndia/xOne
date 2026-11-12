#!/usr/bin/env python3
"""kill -9 the xone producer inside the seal-to-import window (docs/ops/gate-d-rehearsal.md, durability case).

    kill-at-seal.py PID DATADIR OUT

The producer writes `DATADIR/xone/producer/pending.rlp` (synced, renamed into place) before the safety unit signs,
and removes it after the import of the sealed block (services/producer.rs). This polls for the file and sends
SIGKILL the moment it appears, then copies it to OUT for the record. Loopback devnet only.
"""
import os, shutil, signal, sys, time

pid, datadir, out = int(sys.argv[1]), sys.argv[2], sys.argv[3]
p = os.path.join(datadir, "xone", "producer", "pending.rlp")
while not os.path.exists(p):
    pass
os.kill(pid, signal.SIGKILL)
t = time.time()
time.sleep(0.2)
kept = os.path.exists(p)
if kept:
    shutil.copy(p, out)
print(f"killed {pid} at {t:.3f}; pending.rlp {'kept' if kept else 'already removed by the import'}")
