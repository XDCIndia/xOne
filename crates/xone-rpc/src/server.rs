//! The JSON-RPC 2.0 core: parsing, batches, the registry and the ADR-007 section 4 quotas.
//!
//! Method names are exact: `XDPoS_getSigners` and nothing else (geth `rpc/service.go` registers
//! the namespace as written; `xdpos_getSigners` is `-32601`). A method outside the registry, in
//! any namespace, is `-32601` with geth's message (`rpc/errors.go:78-82`).

use crate::{
    api::Backend,
    subscriptions::Hub,
    types::{Params, RpcError, R},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, PoisonError,
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

/// ADR-007 section 4 cost classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cost {
    C0,
    C1,
    C2,
    C3,
}

impl Cost {
    pub fn units(self) -> u64 {
        match self {
            Cost::C0 => 1,
            Cost::C1 => 5,
            Cost::C2 => 20,
            Cost::C3 => 100,
        }
    }
    fn index(self) -> usize {
        match self {
            Cost::C0 => 0,
            Cost::C1 => 1,
            Cost::C2 => 2,
            Cost::C3 => 3,
        }
    }
}

/// What a handler may use.
pub struct Ctx<'a> {
    pub be: &'a Backend,
    /// Set when the client is gone or the deadline passed; long handlers check it.
    pub cancel: &'a AtomicBool,
    pub limits: &'a Limits,
    /// The transport's event hub and polling filters (`filters.rs`).
    pub hub: &'a Hub,
    pub filters: &'a crate::filters::Filters,
}

pub type Handler = Box<dyn Fn(&Ctx, &Params) -> R + Send + Sync>;

#[derive(Default)]
pub struct Registry {
    map: HashMap<&'static str, (Cost, Handler)>,
}

impl Registry {
    pub fn add(
        &mut self,
        name: &'static str,
        cost: Cost,
        f: impl Fn(&Ctx, &Params) -> R + Send + Sync + 'static,
    ) {
        self.map.insert(name, (cost, Box::new(f)));
    }
    pub fn get(&self, name: &str) -> Option<&(Cost, Handler)> {
        self.map.get(name)
    }
    /// Removes `name` and returns its entry (the alias table of `xdc` moves handlers, never copies them).
    pub fn take(&mut self, name: &str) -> Option<(Cost, Handler)> {
        self.map.remove(name)
    }
    pub fn put(&mut self, name: &'static str, entry: (Cost, Handler)) {
        self.map.insert(name, entry);
    }
    /// Every registered name, sorted.
    pub fn names(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.map.keys().copied().collect();
        v.sort_unstable();
        v
    }
}

/// ADR-007 Q12-Q14 and section 4. Defaults are the table's; the node overrides from `[limits]`.
#[derive(Debug, Clone)]
pub struct Limits {
    pub http_connections: usize,
    pub http_waiting: usize,
    pub http_in_flight: usize,
    pub ws_connections: usize,
    pub ws_in_flight: usize,
    pub ws_out_buffer: usize,
    pub ipc_connections: usize,
    pub ipc_in_flight: usize,
    pub body_bytes: usize,
    pub batch_items: usize,
    pub response_bytes: usize,
    /// Concurrency per class C0..C3 (C0 is bounded by the transport's own in-flight limit).
    pub class_concurrency: [usize; 4],
    pub deadline: [Duration; 4],
    pub bucket_burst: u64,
    pub bucket_per_sec: u64,
    pub logs_blocks: u64,
    pub logs_results: usize,
    pub call_gas_cap: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            http_connections: 256,
            http_waiting: 128,
            http_in_flight: 64,
            ws_connections: 128,
            ws_in_flight: 8,
            ws_out_buffer: 1024,
            ipc_connections: 16,
            ipc_in_flight: 8,
            body_bytes: 5 << 20,
            batch_items: 100,
            response_bytes: 20 << 20,
            class_concurrency: [usize::MAX >> 4, 32, 8, 2],
            deadline: [
                Duration::from_secs(2),
                Duration::from_secs(10),
                Duration::from_secs(10),
                Duration::from_secs(30),
            ],
            bucket_burst: 400,
            bucket_per_sec: 200,
            logs_blocks: 5_000,
            logs_results: 10_000,
            call_gas_cap: 50_000_000,
        }
    }
}

/// A token bucket in cost units. The same arithmetic as `xone_node::TokenBucket`, which this crate
/// cannot use (xone-node ranks above xone-rpc in `ci/layering.py`); ponytail: replace with the
/// shared primitive when it moves to a lower crate.
#[derive(Debug)]
pub struct Bucket {
    cap: u64,
    rate: u64,
    level: u64,
    last_ms: u64,
}

impl Bucket {
    pub fn new(burst: u64, per_sec: u64, now_ms: u64) -> Self {
        let cap = burst.saturating_mul(1000);
        Self {
            cap,
            rate: per_sec,
            level: cap,
            last_ms: now_ms,
        }
    }
    /// `Ok` and the cost is taken, or `Err(seconds)` until the bucket holds `cost`.
    pub fn take(&mut self, now_ms: u64, cost: u64) -> Result<(), u64> {
        let dt = now_ms.saturating_sub(self.last_ms);
        self.last_ms = self.last_ms.max(now_ms);
        self.level = self
            .level
            .saturating_add(dt.saturating_mul(self.rate))
            .min(self.cap);
        let need = cost.saturating_mul(1000);
        if need <= self.level {
            self.level -= need;
            return Ok(());
        }
        let missing = need - self.level;
        Err(missing.div_ceil(self.rate.max(1) * 1000).max(1))
    }
}

/// Per-connection state: its bucket and in-flight permits.
pub struct Conn {
    pub bucket: Mutex<Bucket>,
    pub in_flight: Arc<Semaphore>,
    pub id: u64,
}

/// A reply to one request body. `retry_after` is set when the whole call was rejected (HTTP 429).
pub struct Reply {
    pub body: Option<Vec<u8>>,
    pub retry_after: Option<u64>,
}

pub struct Rpc {
    pub be: Backend,
    pub reg: Registry,
    pub limits: Limits,
    /// `Host` and `Origin` checks of the transports (`policy.rs`).
    pub policy: crate::policy::Policy,
    /// Shared with the node, which publishes each canonical block to it.
    pub hub: Arc<Hub>,
    /// Polling filters installed over this transport, fed by `hub`.
    pub filters: crate::filters::Filters,
    class_sem: [Arc<Semaphore>; 4],
    global_bucket: Mutex<Bucket>,
    /// Set by the node's watchdog: C2 and C3 answer `-32005 "node busy"` (ADR-007 section 3, rule 6).
    pub shed: AtomicBool,
    start: Instant,
    next_conn: Mutex<u64>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sets the flag when dropped: the request future was dropped (client gone) or finished.
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl Rpc {
    pub fn new(be: Backend, limits: Limits) -> Arc<Self> {
        let mut reg = Registry::default();
        crate::eth::register(&mut reg);
        crate::net::register(&mut reg);
        crate::web3::register(&mut reg);
        crate::txpool::register(&mut reg);
        crate::admin::register(&mut reg);
        crate::debug::register(&mut reg);
        crate::xdpos::register(&mut reg);
        crate::xdc_ext::register(&mut reg);
        let sem = |i: usize| {
            Arc::new(Semaphore::new(
                limits.class_concurrency[i].min(Semaphore::MAX_PERMITS),
            ))
        };
        Arc::new(Self {
            be,
            reg,
            class_sem: [sem(0), sem(1), sem(2), sem(3)],
            global_bucket: Mutex::new(Bucket::new(limits.bucket_burst, limits.bucket_per_sec, 0)),
            limits,
            policy: Default::default(),
            hub: Arc::default(),
            filters: Default::default(),
            shed: AtomicBool::new(false),
            start: Instant::now(),
            next_conn: Mutex::new(0),
        })
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub fn new_conn(&self, in_flight: usize) -> Conn {
        let mut n = lock(&self.next_conn);
        *n += 1;
        Conn {
            bucket: Mutex::new(Bucket::new(
                self.limits.bucket_burst,
                self.limits.bucket_per_sec,
                self.now_ms(),
            )),
            in_flight: Arc::new(Semaphore::new(in_flight)),
            id: *n,
        }
    }

    fn cost_of(&self, method: &str) -> Cost {
        // ADR-007: default C1 for an unknown method.
        self.reg.get(method).map_or(Cost::C1, |(c, _)| *c)
    }

    /// One request object to its response; `None` for a notification.
    fn one(&self, req: &Value, cancel: &AtomicBool) -> Option<Value> {
        let id = req.get("id").cloned();
        let fail = |id: Option<Value>, e: RpcError| {
            Some(json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": e.to_value()}))
        };
        let Some(obj) = req.as_object() else {
            return fail(None, RpcError::invalid_request("invalid request"));
        };
        let method = obj.get("method").and_then(Value::as_str);
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
            return fail(id, RpcError::invalid_request("invalid request"));
        }
        let method = method.unwrap_or("");
        let notification = id.is_none();
        let res = self.call(method, obj.get("params"), cancel);
        if notification {
            return None;
        }
        Some(match res {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e.to_value()}),
        })
    }

    /// Runs `method` with positional `params`.
    pub fn call(&self, method: &str, params: Option<&Value>, cancel: &AtomicBool) -> R {
        let Some((cost, h)) = self.reg.get(method) else {
            // eth_subscribe over a transport that cannot push (geth `notificationsUnsupportedError`).
            if method == "eth_subscribe" || method == "eth_unsubscribe" {
                return Err(RpcError::new(-32601, "notifications not supported"));
            }
            return Err(RpcError::method_not_found(method));
        };
        if *cost >= Cost::C2 && self.shed.load(Ordering::Relaxed) {
            return Err(RpcError::limit(json!("node busy")));
        }
        let empty = Vec::new();
        let v = match params {
            None | Some(Value::Null) => &empty,
            Some(Value::Array(a)) => a,
            Some(_) => return Err(RpcError::invalid_params("non-array args")),
        };
        let ctx = Ctx {
            be: &self.be,
            cancel,
            limits: &self.limits,
            hub: &self.hub,
            filters: &self.filters,
        };
        h(
            &ctx,
            &Params {
                v,
                head: self.be.chain.head().0,
            },
        )
    }

    /// Dispatches a parsed message (a request or a batch) synchronously. No quotas.
    pub fn handle_value(&self, v: &Value, cancel: &AtomicBool) -> Option<Value> {
        match v {
            Value::Array(items) => {
                if items.is_empty() {
                    return Some(
                        json!({"jsonrpc": "2.0", "id": null, "error": RpcError::invalid_request("empty batch").to_value()}),
                    );
                }
                if items.len() > self.limits.batch_items {
                    return Some(
                        json!({"jsonrpc": "2.0", "id": null, "error": RpcError::invalid_request("batch too large").to_value()}),
                    );
                }
                let out: Vec<Value> = items.iter().filter_map(|r| self.one(r, cancel)).collect();
                (!out.is_empty()).then_some(Value::Array(out))
            }
            other => self.one(other, cancel),
        }
    }

    /// `handle_value` on bytes, without quotas (tests, tools).
    pub fn handle_bytes(&self, body: &[u8]) -> Option<Vec<u8>> {
        let no_cancel = AtomicBool::new(false);
        let out = match serde_json::from_slice::<Value>(body) {
            Ok(v) => self.handle_value(&v, &no_cancel)?,
            Err(_) => {
                json!({"jsonrpc": "2.0", "id": null, "error": RpcError::new(-32700, "parse error").to_value()})
            }
        };
        serde_json::to_vec(&out).ok()
    }

    fn rejected(&self, e: RpcError, retry_after: Option<u64>) -> Reply {
        let v = json!({"jsonrpc": "2.0", "id": null, "error": e.to_value()});
        Reply {
            body: serde_json::to_vec(&v).ok(),
            retry_after,
        }
    }

    /// One request body through the quotas: the bucket of the connection and of the transport, the
    /// class concurrency, the class deadline and cancellation on drop. The caller holds the
    /// transport's in-flight permit.
    pub async fn serve(self: &Arc<Self>, conn: &Conn, body: Vec<u8>) -> Reply {
        if body.len() > self.limits.body_bytes {
            return self.rejected(RpcError::invalid_request("content length too large"), None);
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
            return self.rejected(RpcError::new(-32700, "parse error"), None);
        };
        let mut cost = Cost::C0;
        let mut units = 0u64;
        let methods: Vec<&str> = match &msg {
            Value::Array(a) => a
                .iter()
                .map(|r| r.get("method").and_then(Value::as_str).unwrap_or(""))
                .collect(),
            r => vec![r.get("method").and_then(Value::as_str).unwrap_or("")],
        };
        if methods.len() > self.limits.batch_items {
            return self.rejected(RpcError::invalid_request("batch too large"), None);
        }
        for m in &methods {
            let c = self.cost_of(m);
            cost = cost.max(c);
            units += c.units();
        }
        // The batch counts as the sum of its parts (ADR-007 section 4).
        let now = self.now_ms();
        let taken = lock(&conn.bucket)
            .take(now, units)
            .and_then(|()| lock(&self.global_bucket).take(now, units));
        if let Err(secs) = taken {
            return self.rejected(RpcError::limit(json!(secs)), Some(secs));
        }
        let _permit = if cost == Cost::C0 {
            None
        } else {
            match self.class_sem[cost.index()].clone().try_acquire_owned() {
                Ok(p) => Some(p),
                Err(_) => return self.rejected(RpcError::limit(json!(1)), Some(1)),
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let _guard = CancelOnDrop(cancel.clone());
        let me = self.clone();
        let flag = cancel.clone();
        let job = tokio::task::spawn_blocking(move || me.handle_value(&msg, &flag));
        let out = match tokio::time::timeout(self.limits.deadline[cost.index()], job).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": RpcError::internal("handler panicked").to_value()}),
            ),
            Err(_) => {
                cancel.store(true, Ordering::Relaxed);
                Some(
                    json!({"jsonrpc": "2.0", "id": null, "error": RpcError::server("request timed out").to_value()}),
                )
            }
        };
        let body = out.and_then(|v| serde_json::to_vec(&v).ok()).map(|b| {
            if b.len() > self.limits.response_bytes {
                let e = json!({"jsonrpc": "2.0", "id": null, "error": RpcError::new(-32003, "response too large").to_value()});
                serde_json::to_vec(&e).unwrap_or_else(|_| b"null".to_vec())
            } else {
                b
            }
        });
        Reply {
            body,
            retry_after: None,
        }
    }
}
