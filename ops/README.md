# Running xone on a host

`run-xone.sh` and `xone@.service` keep the env contract of xOneGo's `ops/deploy/run-xone.sh`:
`NETWORK`, `DATADIR`, `NODENAME`, `ETHSTATS_HOST`, `ETHSTATS_SECRET`, `P2P_PORT`, `HTTP_PORT`,
`AUTH_PORT`, `EXTRA_ARGS`, and the per-network file `/etc/xone/<net>.env`. The launch line
uses xone flags (`docs/cli/flags.md`) instead of xone-geth flags.

```bash
install -m755 ops/run-xone.sh /opt/xone/run-xone.sh
install -m644 ops/xone@.service /etc/systemd/system/xone@.service
install -d -m750 /etc/xone
install -m600 /dev/null /etc/xone/apothem.env     # then edit it, see below
systemctl enable --now xone@apothem
```

`/etc/xone/apothem.env`:

```
DATADIR=/data/xone/apothem
ETHSTATS_SECRET=<from the fleet secret store>
# EXTRA_ARGS=--mine --miner.etherbase 0x... --password /etc/xone/pw
```

## Secrets

* The ethstats secret comes only from the env file (or the caller's environment). The
  xOneGo script had a default value for `ETHSTATS_SECRET` in the script text; this one has
  none. Without a secret the node starts without `--ethstats` and logs a warning, which
  means it will not show on the stats board.
* The secret is part of the `--ethstats name:secret@host:port` argument, so it is visible
  in the process list and in `xone dumpconfig` output. Do not paste either into tickets.
* The password for `--mine` is a file path (`--password`), never a value. Keystores are
  not touched by these scripts.
* `jwt.hex` is created in the datadir with mode 600 on first start.

## Stop

`systemctl stop xone@<net>` sends SIGTERM. The node stops its services in reverse order,
each with a deadline, and exits within `limits.stop_timeout_ms` (5 s). More SIGTERMs during
the stop do nothing. Exit code 0 is a clean stop, 3 means a service did not stop in time
(the name is on stderr), 1 is a start-up error (for example a datadir made for another chain).

## Datadir and chain

The first start (or `xone --datadir D init genesis.json`) writes `D/xone-chain.json` with
the chain id and the genesis hash. A later start for a different chain exits 1 and leaves
the datadir alone. Move the datadir aside by hand to change chains.

## Not covered yet

* The node starts no p2p, sync or RPC service yet (XB-19 is the CLI, config and lifecycle).
  `systemctl start` therefore runs a node that only holds the datadir and waits for SIGTERM.
* Custom chains: `init` and start accept only genesis files whose chain id is one of the
  four built in (mainnet 50, apothem 51, devnet 5551, devnet551); other chain specs need a
  chain-spec loader in `xone-chainspec`.
* The xOneGo `NETWORK=net5050` branch (auto-download of genesis and bootnodes) is not ported;
  pass `GENESIS`, `CHAINID` and `BOOTNODES` for a custom chain.
