//! Polling filters (geth `eth/filters/api.go`): `eth_newFilter`, `eth_newBlockFilter`,
//! `eth_newPendingTransactionFilter`, `eth_getFilterChanges`, `eth_getFilterLogs` and
//! `eth_uninstallFilter`, on every transport.
//!
//! Block and log filters subscribe to the transport's event hub ([`Hub`]), which the node feeds
//! with each canonical block and, on a reorg, the removed logs (`removed: true`). Each filter's
//! buffer is the hub queue, bounded to [`BUFFER`] events: when it is full the hub drops the
//! filter and the next poll answers an error, never a silent gap. The pending filter compares the
//! pool's content between polls (the node publishes no pending-transaction events): it reports
//! the transactions that entered since the previous poll and still are in the pool.
//!
//! A filter not polled for [`TIMEOUT`] is removed (geth's 5-minute deadline); the table is swept
//! on every filter call. At most [`MAX_FILTERS`] filters exist per transport. Filter ids are 16
//! random bytes, so one client cannot guess another's filter. Filters belong to the transport
//! (HTTP, WebSocket or IPC endpoint) that installed them.

use crate::{
    eth::get_logs,
    json::{h256, tx_json},
    server::{Cost, Ctx, Registry},
    subscriptions::{Hub, Kind, LogFilter},
    types::{parse_quantity, Params, RpcError, R},
};
use alloy_primitives::{keccak256, B256};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};
use tokio::sync::mpsc::{self, error::TryRecvError, Receiver};

/// geth `filters.FilterAPI` timeout (`eth/filters/api.go`, `--rpc.filtertimeout` default).
pub const TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Events a block or log filter holds between polls (the ADR-007 Q13 queue size).
pub const BUFFER: usize = 1024;
/// Filters per transport.
pub const MAX_FILTERS: usize = 1024;
/// The hub connection id of the filters: no client connection has it, so a closed connection
/// never removes a filter.
const FILTER_CONN: u64 = u64::MAX;
/// geth `maxTopics`.
const MAX_TOPICS: usize = 4;

enum Feed {
    /// The hub subscription id and its queue.
    Hub { sub: String, rx: Receiver<String> },
    /// The pool's transaction hashes at the last poll.
    Pending { seen: BTreeSet<B256>, full_tx: bool },
}

enum Ty {
    Blocks,
    /// The criteria object as given (for `eth_getFilterLogs`) and the absolute block bounds.
    Logs {
        crit: Map<String, Value>,
        from: Option<u64>,
        to: Option<u64>,
    },
    Pending,
}

struct Filter {
    ty: Ty,
    feed: Feed,
    polled: Instant,
}

#[derive(Default)]
pub struct Filters {
    table: Mutex<HashMap<String, Filter>>,
}

fn not_found() -> RpcError {
    RpcError::server("filter not found")
}

fn new_id() -> Result<String, RpcError> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| RpcError::internal(format!("filter id: {e}")))?;
    Ok(format!("0x{}", alloy_primitives::hex::encode(b)))
}

impl Filters {
    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<String, Filter>> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Removes the filters idle for [`TIMEOUT`] at `now`, and their hub subscriptions.
    fn sweep(&self, hub: &Hub, now: Instant) {
        let mut t = self.table();
        t.retain(|_, f| {
            let live = now.saturating_duration_since(f.polled) < TIMEOUT;
            if !live {
                if let Feed::Hub { sub, .. } = &f.feed {
                    hub.unsubscribe(FILTER_CONN, sub);
                }
            }
            live
        });
    }

    fn install(&self, hub: &Hub, ty: Ty, feed: Feed, now: Instant) -> R {
        self.sweep(hub, now);
        let mut t = self.table();
        if t.len() >= MAX_FILTERS {
            if let Feed::Hub { sub, .. } = &feed {
                hub.unsubscribe(FILTER_CONN, sub);
            }
            return Err(RpcError::limit(json!(format!(
                "too many filters (at most {MAX_FILTERS})"
            ))));
        }
        let id = new_id()?;
        t.insert(
            id.clone(),
            Filter {
                ty,
                feed,
                polled: now,
            },
        );
        Ok(json!(id))
    }

    fn subscribe(hub: &Hub, kind: Kind) -> Feed {
        let (tx, rx) = mpsc::channel(BUFFER);
        let sub = hub.subscribe(FILTER_CONN, kind, tx);
        Feed::Hub { sub, rx }
    }

    pub fn uninstall(&self, hub: &Hub, id: &str, now: Instant) -> bool {
        self.sweep(hub, now);
        match self.table().remove(id) {
            Some(f) => {
                if let Feed::Hub { sub, .. } = &f.feed {
                    hub.unsubscribe(FILTER_CONN, sub);
                }
                true
            }
            None => false,
        }
    }

    /// `eth_getFilterChanges`: what arrived since the previous poll.
    fn changes(&self, c: &Ctx, id: &str, now: Instant) -> R {
        self.sweep(c.hub, now);
        let mut t = self.table();
        let f = t.get_mut(id).ok_or_else(not_found)?;
        f.polled = now;
        let out = match &mut f.feed {
            Feed::Hub { rx, .. } => {
                let mut items = Vec::new();
                let overflow = loop {
                    match rx.try_recv() {
                        Ok(msg) => items.push(msg),
                        Err(TryRecvError::Empty) => break false,
                        // the hub dropped the filter: its buffer was full
                        Err(TryRecvError::Disconnected) => break true,
                    }
                };
                if overflow {
                    t.remove(id);
                    return Err(RpcError::server(format!(
                        "filter buffer overflow: more than {BUFFER} events between polls; install a new filter"
                    )));
                }
                items
                    .iter()
                    .filter_map(|m| serde_json::from_str::<Value>(m).ok())
                    .filter_map(|m| m.get("params")?.get("result").cloned())
                    .filter_map(|r| match &f.ty {
                        Ty::Blocks => r.get("hash").cloned(),
                        Ty::Logs { from, to, .. } => in_range(&r, *from, *to).then_some(r),
                        Ty::Pending => None,
                    })
                    .collect()
            }
            Feed::Pending { seen, full_tx } => {
                let (pending, queued) = c.be.pool.content();
                let id = c.be.chain.spec().chain_id;
                let mut now_seen = BTreeSet::new();
                let mut out = Vec::new();
                for raw in pending.values().chain(queued.values()).flatten() {
                    let h = keccak256(raw);
                    if now_seen.insert(h) && !seen.contains(&h) {
                        out.push(if *full_tx {
                            tx_json(raw, id, None)?
                        } else {
                            h256(h)
                        });
                    }
                }
                *seen = now_seen;
                out
            }
        };
        Ok(Value::Array(out))
    }

    /// The stored criteria of log filter `id`.
    fn criteria(&self, hub: &Hub, id: &str, now: Instant) -> Result<Map<String, Value>, RpcError> {
        self.sweep(hub, now);
        let t = self.table();
        match t.get(id).map(|f| &f.ty) {
            Some(Ty::Logs { crit, .. }) => Ok(crit.clone()),
            _ => Err(not_found()),
        }
    }

    pub fn len(&self) -> usize {
        self.table().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// geth `filterLogs`: only absolute block numbers bound a live log filter.
fn in_range(log: &Value, from: Option<u64>, to: Option<u64>) -> bool {
    let Some(n) = log
        .get("blockNumber")
        .and_then(Value::as_str)
        .and_then(|s| parse_quantity(s).ok())
    else {
        return true;
    };
    from.is_none_or(|f| n >= f) && to.is_none_or(|t| n <= t)
}

/// A block bound of the criteria: a number is absolute; a tag is open; `pending` is refused.
fn bound(o: &Map<String, Value>, k: &str) -> Result<Option<u64>, RpcError> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => match s.as_str() {
            "pending" => Err(RpcError::invalid_params("pending logs are not supported")),
            "latest" | "earliest" | "safe" | "finalized" => Ok(None),
            s => parse_quantity(s).map(Some),
        },
        Some(_) => Err(RpcError::invalid_params("invalid block range params")),
    }
}

fn new_filter(c: &Ctx, p: &Params) -> R {
    let o = p.obj(0)?.clone();
    if o.get("topics")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
        > MAX_TOPICS
    {
        return Err(RpcError::invalid_params("exceed max topics"));
    }
    let has_range = o.get("fromBlock").is_some_and(|v| !v.is_null())
        || o.get("toBlock").is_some_and(|v| !v.is_null());
    if o.get("blockHash").is_some_and(|v| !v.is_null()) && has_range {
        return Err(RpcError::invalid_params(
            "cannot specify both BlockHash and FromBlock/ToBlock, choose one or the other",
        ));
    }
    let lf = LogFilter::parse(&o)?;
    let (from, to) = (bound(&o, "fromBlock")?, bound(&o, "toBlock")?);
    if let (Some(f), Some(t)) = (from, to) {
        if f > t {
            return Err(RpcError::invalid_params("invalid block range params"));
        }
    }
    let feed = Filters::subscribe(c.hub, Kind::Logs(lf));
    c.filters
        .install(c.hub, Ty::Logs { crit: o, from, to }, feed, Instant::now())
}

fn new_block_filter(c: &Ctx, _: &Params) -> R {
    let feed = Filters::subscribe(c.hub, Kind::NewHeads);
    c.filters.install(c.hub, Ty::Blocks, feed, Instant::now())
}

fn new_pending_filter(c: &Ctx, p: &Params) -> R {
    let full_tx = p.opt(0).and_then(Value::as_bool).unwrap_or(false);
    let (pending, queued) = c.be.pool.content();
    let seen = pending
        .values()
        .chain(queued.values())
        .flatten()
        .map(keccak256)
        .collect();
    c.filters.install(
        c.hub,
        Ty::Pending,
        Feed::Pending { seen, full_tx },
        Instant::now(),
    )
}

fn get_filter_logs(c: &Ctx, p: &Params) -> R {
    let crit = c.filters.criteria(c.hub, p.str(0)?, Instant::now())?;
    let v = [Value::Object(crit)];
    get_logs(
        c,
        &Params {
            v: &v,
            head: p.head,
        },
    )
}

pub fn register(r: &mut Registry) {
    r.add("eth_newFilter", Cost::C1, new_filter);
    r.add("eth_newBlockFilter", Cost::C1, new_block_filter);
    r.add(
        "eth_newPendingTransactionFilter",
        Cost::C1,
        new_pending_filter,
    );
    r.add("eth_getFilterChanges", Cost::C1, |c, p| {
        c.filters.changes(c, p.str(0)?, Instant::now())
    });
    r.add("eth_getFilterLogs", Cost::C2, get_filter_logs);
    r.add("eth_uninstallFilter", Cost::C1, |c, p| {
        Ok(json!(c.filters.uninstall(c.hub, p.str(0)?, Instant::now())))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_filters_expire_after_the_timeout() {
        let hub = Hub::default();
        let f = Filters::default();
        let t0 = Instant::now();
        let feed = Filters::subscribe(&hub, Kind::NewHeads);
        let id = f.install(&hub, Ty::Blocks, feed, t0).ok();
        let id = id
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(String::new);
        assert_eq!((f.len(), hub.len()), (1, 1));
        // just before the deadline it is kept, at the deadline it is gone with its subscription
        f.sweep(&hub, t0 + TIMEOUT - Duration::from_secs(1));
        assert_eq!(f.len(), 1);
        f.sweep(&hub, t0 + TIMEOUT);
        assert_eq!((f.len(), hub.len()), (0, 0));
        assert!(!f.uninstall(&hub, &id, t0 + TIMEOUT));
    }

    #[test]
    fn the_filter_count_is_capped() {
        let hub = Hub::default();
        let f = Filters::default();
        let now = Instant::now();
        for _ in 0..MAX_FILTERS {
            let feed = Filters::subscribe(&hub, Kind::NewHeads);
            assert!(f.install(&hub, Ty::Blocks, feed, now).is_ok());
        }
        let feed = Filters::subscribe(&hub, Kind::NewHeads);
        let e = f.install(&hub, Ty::Blocks, feed, now);
        assert_eq!(e.err().map(|e| e.code), Some(-32005));
        assert_eq!((f.len(), hub.len()), (MAX_FILTERS, MAX_FILTERS));
    }

    #[test]
    fn range_bounds_only_absolute_numbers() {
        let l = json!({"blockNumber": "0x10"});
        assert!(in_range(&l, None, None));
        assert!(in_range(&l, Some(0x10), Some(0x10)));
        assert!(!in_range(&l, Some(0x11), None));
        assert!(!in_range(&l, None, Some(0xf)));
    }
}
