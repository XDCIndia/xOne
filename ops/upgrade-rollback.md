# Upgrade N to N+1, and back

Hosts: followers first. A producer (signer) host is upgraded only with the owner present, one at a time.
Stamps below are `xone --version`. Do every step on one host, then repeat. The rehearsal that proves these
steps on a loopback devnet is in `docs/ops/release-rehearsal.md`.

## Rules that decide the path

1. **Datadir format version (ADR-005 section 12).** The binary refuses a datadir whose format version it
   does not know: `FormatVersion(found)` (D5). An older datadir under a newer binary needs a migration
   (`xone db migrate`, offline, resumable, the manifest is written last). A newer datadir under an older
   binary is **always refused**: there is no in-place downgrade. So the release notes of N+1 must say
   "datadir format: unchanged" or "datadir format: N -> N+1, migration required". Read that line first.
   - Format unchanged: rollback is "install the old binary and start".
   - Format changed: rollback is "restore the backup taken before the upgrade". Without a backup there is no
     way back except a resync.
2. **Safety record (ADR-010 section 5).** `datadir/safety/` is the anti-equivocation record; it only
   rises. A rollback **never lowers it**: do not delete `datadir/safety/`, do not restore it from a backup,
   do not copy it between hosts. The restore in step B3 skips `safety/` when the host signs. After a
   chain rollback the signer may refuse to vote in rounds it re-visits; that is the intended cost. Any
   restore that would bring back an older `safety/` is an operator action outside the node and the node
   treats the next start as unproven (ADR-010 section 8). On a follower there is no signer and no record.
3. One signer at a time. Never start the old and the new unit together on one datadir (the `flock` on
   `datadir/safety/LOCK` stops a second process on one host; it does not stop two hosts).

## A. Upgrade

```bash
V=<N+1>                                            # e.g. 0.2.0
# A1 verify the new release exactly as in ops/install.md steps 1-5, in a scratch directory
# A2 note the state before
curl -s -X POST -H 'content-type: application/json' --data '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' 127.0.0.1:8545
/opt/xone/xone --version                           # the stamp of N
# A3 stop, back up, swap
systemctl stop xone@apothem                        # exit 0 within 15 s; check: systemctl is-active xone@apothem
cp -a /data/xone/apothem /data/xone/apothem.pre-$V # whole datadir, node stopped (safety/ included on a signer)
mv /opt/xone /opt/xone.N && install -d /opt/xone && cp -r xone-$V-linux-x86_64/. /opt/xone/
/opt/xone/xone --version                           # the stamp of N+1
# A4 start and prove
systemctl start xone@apothem
journalctl -u xone@apothem -n 20                   # no FormatVersion, no refused datadir
# head is at or above the value in A2, and rises; web3_clientVersion equals the new stamp
```

The backup needs the same free space as the datadir. Keep `/opt/xone.N` and the backup until N+1 has run
one epoch.

## B. Rollback

```bash
# B1 stop the new binary
systemctl stop xone@apothem
# B2 format unchanged: put the old binary back and start
rm -r /opt/xone && mv /opt/xone.N /opt/xone && systemctl start xone@apothem
# B3 format changed (the new binary migrated): restore the pre-upgrade copy instead
mv /data/xone/apothem /data/xone/apothem.failed
cp -a /data/xone/apothem.pre-$V /data/xone/apothem
#   on a signer: keep the CURRENT safety record, it is never lowered
rm -rf /data/xone/apothem/safety 2>/dev/null; cp -a /data/xone/apothem.failed/safety /data/xone/apothem/safety 2>/dev/null || true
rm -r /opt/xone && mv /opt/xone.N /opt/xone && systemctl start xone@apothem
# B4 prove: stamp of N, head resumes and rises, no safety/ file is older than before the upgrade
```

The follower datadir keeps `xone-chain.json` (chain id and genesis hash) and `xone/` (segments, `head.json`,
checkpoints). A restart after B2 or B3 replays from the newest checkpoint that belongs to the stored chain
and catches up by peers; it never starts again at block 0 on a datadir that holds blocks.

## C. When to stop and ask

* `FormatVersion` on start of N+1: the datadir is older than N+1 knows and no migration ran. Do B2/B3, then
  read the release notes.
* `FormatVersion` on start of N after rollback: the datadir is newer. Use B3. Do not edit the manifest.
* The head does not rise after 5 minutes with peers connected: roll back (B), keep the failed datadir
  (`apothem.failed`) and the journal for the engineers.
* Any step that would lower, delete or replace `safety/`: stop. Ask the owner.

## D. Check list (record each upgrade)

| Item | Value |
|---|---|
| Host, network | |
| Stamp N, head before | |
| Stamp N+1, datadir format line from the notes | |
| Backup path and size | |
| Head after start, after 10 minutes | |
| Rollback tried (yes/no), result | |
| `safety/` unchanged or higher (signer only) | |
