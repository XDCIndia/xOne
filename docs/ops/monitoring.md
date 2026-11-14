# Monitoring: Prometheus and Grafana

The node serves Prometheus text on `--metrics --metrics.addr A --metrics.port P` (default port
6060): `/metrics`, `/livez`, `/readyz`. The metric families are listed in
[spec/ops/metrics.md](../../spec/ops/metrics.md) and produced by
`crates/xone-node/src/observe/metrics.rs`.

## Shipped files

| File | Content |
|---|---|
| `ops/grafana/xone-overview.json` | Grafana dashboard "xOne overview" (uid `xone-overview`): head and head age, peers, import rate, syncing, required-block refusals, txpool, RPC in flight per ADR-007 class, queue depth and overflows, consensus round and QC round, round timeouts, missed eligible turns, storage, watchdog lag, dropped log lines. Variables: data source and instance. |
| `ops/prometheus/alerts.yml` | Alert rules: head older than 30 s, fewer than 3 peers, missed eligible turns, queue overflow, round timeouts, datadir above 85 % of the disk (needs node_exporter), `/metrics` down. |
| `ops/prometheus/prometheus.yml` | Scrape config for the local stack (job `xone`). |
| `ops/grafana/docker-compose.yml` | Local Prometheus and Grafana with the data source and the dashboard provisioned. |

## Local stack

1. Start the node with `--metrics --metrics.addr 0.0.0.0`.
2. Set the target in `ops/prometheus/prometheus.yml` if the node does not listen on
   `host.docker.internal:6060`.
3. `docker compose -f ops/grafana/docker-compose.yml up -d`
4. Open Grafana on http://localhost:3000 (first login admin/admin); the dashboard is "xOne overview".
   Prometheus alerts are on http://localhost:9090/alerts.

To import into an existing Grafana, upload `ops/grafana/xone-overview.json` and choose the
Prometheus data source; load `ops/prometheus/alerts.yml` as a rule file in Prometheus and route
it through your Alertmanager.

## Disk alert

`XoneDiskAlmostFull` divides `xone_storage_bytes` by node_exporter's
`node_filesystem_size_bytes` for the datadir mount. Change `mountpoint="/var/lib/xone"` to your
mount, or delete the rule if node_exporter does not run on the host.

## Check

`cargo test -p xone-node --test monitoring` fails when the dashboard or the alert rules name a
metric the node does not export, so a renamed family cannot leave a silent empty panel.
