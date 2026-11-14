//! HTTP (hyper), WebSocket (tokio-tungstenite) and IPC (a Unix socket carrying a JSON stream).
//! Each listener bounds its connections (ADR-007 Q12-Q14), and every request goes through
//! [`Rpc::serve`], so the quotas, deadlines and cancellation are the same on all three. A request
//! whose connection closes is cancelled: its future is dropped and the cancel flag is set.

use crate::{
    server::{Conn, Rpc},
    subscriptions::{parse_kind, SubRefused},
    types::RpcError,
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::Bytes, service::service_fn, Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::{
    convert::Infallible,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixListener},
    sync::{mpsc, Semaphore},
    task::JoinSet,
};
use tokio_tungstenite::tungstenite::{
    handshake::server::{ErrorResponse, Request as WsRequest, Response as WsResponse},
    protocol::WebSocketConfig,
    Message,
};

struct HttpState {
    rpc: Arc<Rpc>,
    in_flight: Arc<Semaphore>,
    waiting: AtomicUsize,
}

fn http_reply(status: StatusCode, body: Vec<u8>, retry: Option<u64>) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::from(body)));
    *r.status_mut() = status;
    r.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json"),
    );
    if let Some(s) = retry {
        if let Ok(v) = hyper::header::HeaderValue::from_str(&s.to_string()) {
            r.headers_mut().insert("retry-after", v);
        }
    }
    r
}

async fn http_handle(
    st: Arc<HttpState>,
    conn: Arc<Conn>,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let limits = &st.rpc.limits;
    let host = req.headers().get("host").and_then(|h| h.to_str().ok());
    if let Err(e) = st.rpc.policy.check_host(host) {
        return Ok(http_reply(
            StatusCode::FORBIDDEN,
            e.to_string().into_bytes(),
            None,
        ));
    }
    // authentication before anything else is read or dispatched
    let auth = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok());
    if let Err(e) = st.rpc.policy.check_auth(auth) {
        return Ok(http_reply(
            StatusCode::UNAUTHORIZED,
            e.to_string().into_bytes(),
            None,
        ));
    }
    if req.method() != Method::POST {
        return Ok(http_reply(StatusCode::METHOD_NOT_ALLOWED, vec![], None));
    }
    // ADR-007 Q12: at most `http_waiting` requests wait for one of `http_in_flight` slots.
    let permit = match st.in_flight.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            if st.waiting.fetch_add(1, Ordering::Relaxed) >= limits.http_waiting {
                st.waiting.fetch_sub(1, Ordering::Relaxed);
                let e = json!({"jsonrpc": "2.0", "id": null, "error": RpcError::limit(json!(1)).to_value()});
                return Ok(http_reply(
                    StatusCode::TOO_MANY_REQUESTS,
                    e.to_string().into_bytes(),
                    Some(1),
                ));
            }
            let p = st.in_flight.clone().acquire_owned().await;
            st.waiting.fetch_sub(1, Ordering::Relaxed);
            match p {
                Ok(p) => p,
                Err(_) => return Ok(http_reply(StatusCode::SERVICE_UNAVAILABLE, vec![], None)),
            }
        }
    };
    let body = match Limited::new(req.into_body(), limits.body_bytes)
        .collect()
        .await
    {
        Ok(b) => b.to_bytes().to_vec(),
        Err(_) => return Ok(http_reply(StatusCode::PAYLOAD_TOO_LARGE, vec![], None)),
    };
    let reply = st.rpc.serve(&conn, body).await;
    drop(permit);
    let status = if reply.retry_after.is_some() {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        StatusCode::OK
    };
    Ok(http_reply(
        status,
        reply.body.unwrap_or_else(Vec::new),
        reply.retry_after,
    ))
}

/// Serves HTTP on `listener` until the future is dropped. Beyond `http_connections` open
/// connections a new one is closed at once.
pub async fn serve_http(rpc: Arc<Rpc>, listener: TcpListener) {
    let conns = Arc::new(Semaphore::new(rpc.limits.http_connections));
    let st = Arc::new(HttpState {
        in_flight: Arc::new(Semaphore::new(rpc.limits.http_in_flight)),
        waiting: AtomicUsize::new(0),
        rpc: rpc.clone(),
    });
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let Ok(slot) = conns.clone().try_acquire_owned() else {
            continue;
        };
        let (st, conn) = (st.clone(), Arc::new(rpc.new_conn(1)));
        tokio::spawn(async move {
            let svc = service_fn(move |req| http_handle(st.clone(), conn.clone(), req));
            // A dropped connection drops the in-progress request future, which cancels it.
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), svc)
                .await;
            drop(slot);
        });
    }
}

/// One WebSocket or IPC connection: subscriptions, and requests with the in-flight bound.
struct Session {
    rpc: Arc<Rpc>,
    conn: Arc<Conn>,
    out: mpsc::Sender<String>,
    tasks: JoinSet<()>,
}

impl Session {
    fn new(rpc: &Arc<Rpc>, in_flight: usize) -> (Self, mpsc::Receiver<String>) {
        let (out, rx) = mpsc::channel(rpc.limits.ws_out_buffer);
        (
            Self {
                rpc: rpc.clone(),
                conn: Arc::new(rpc.new_conn(in_flight)),
                out,
                tasks: JoinSet::new(),
            },
            rx,
        )
    }

    /// Whether `v` is an `eth_subscribe` or `eth_unsubscribe` request.
    fn is_sub(v: &Value) -> bool {
        matches!(
            v.get("method").and_then(Value::as_str),
            Some("eth_subscribe" | "eth_unsubscribe")
        )
    }

    /// One `eth_subscribe` or `eth_unsubscribe` request through the same admission as other
    /// methods (#267): the JSON-RPC envelope, the transport's namespaces, the token buckets, and the
    /// subscription bounds. The reply (`None` for a notification) and the subscription to activate
    /// once the reply is queued.
    fn subscription(&self, msg: &Value) -> (Option<Value>, Option<String>) {
        let id = msg.get("id").cloned();
        let reply = |r: Result<Value, RpcError>| {
            let id = id.clone().unwrap_or(Value::Null);
            Some(match r {
                Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
                Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e.to_value()}),
            })
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return (
                reply(Err(RpcError::invalid_request("invalid request"))),
                None,
            );
        }
        // a transport without the eth namespace serves no eth subscriptions
        if !self.rpc.reg.has_namespace("eth") {
            return (reply(Err(RpcError::method_not_found(method))), None);
        }
        if let Err(secs) = self.rpc.take_subscription(&self.conn) {
            return (reply(Err(RpcError::limit(json!(secs)))), None);
        }
        let notification = id.is_none();
        let params: &[Value] = msg
            .get("params")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice);
        if method == "eth_unsubscribe" {
            let done = params
                .first()
                .and_then(Value::as_str)
                .is_some_and(|s| self.rpc.hub.unsubscribe(self.conn.id, s));
            return (
                if notification {
                    None
                } else {
                    reply(Ok(json!(done)))
                },
                None,
            );
        }
        // a subscription nobody can learn the id of is not created
        if notification {
            return (None, None);
        }
        let kind = match parse_kind(params) {
            Ok(k) => k,
            Err(e) => return (reply(Err(e)), None),
        };
        let lim = &self.rpc.limits;
        match self.rpc.hub.subscribe_bounded(
            self.conn.id,
            kind,
            self.out.clone(),
            lim.subs_per_conn,
            lim.subs_total,
        ) {
            Ok(sid) => (reply(Ok(json!(sid))), Some(sid)),
            Err(r) => {
                let why = match r {
                    SubRefused::PerConnection(n) => {
                        format!("too many subscriptions on this connection (max {n})")
                    }
                    SubRefused::Total(n) => format!("too many subscriptions (max {n})"),
                };
                (reply(Err(RpcError::limit(json!(why)))), None)
            }
        }
    }

    /// Queues `reply`, then starts the notifications of `activate`: no event precedes the
    /// subscription's reply.
    async fn answer(&self, reply: Option<Value>, activate: Vec<String>) {
        let reply = reply.map(|r| r.to_string());
        if activate.is_empty() {
            if let Some(r) = reply {
                let _ = self.out.send(r).await;
            }
            return;
        }
        // the reply and the activation are one step under the hub's lock
        let _ = self
            .rpc
            .hub
            .reply_then_activate(self.conn.id, &activate, &self.out, reply);
    }

    async fn on_message(&mut self, text: String) {
        while self.tasks.try_join_next().is_some() {}
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            match &v {
                Value::Object(_) if Self::is_sub(&v) => {
                    let (reply, act) = self.subscription(&v);
                    self.answer(reply, act.into_iter().collect()).await;
                    return;
                }
                // a batch of only subscription requests: each one admitted like a single one,
                // one array reply without the notifications' entries
                Value::Array(items)
                    if !items.is_empty()
                        && items.len() <= self.rpc.limits.batch_items
                        && items.iter().all(Self::is_sub) =>
                {
                    let (mut replies, mut act) = (vec![], vec![]);
                    for it in items {
                        let (r, a) = self.subscription(it);
                        replies.extend(r);
                        act.extend(a);
                    }
                    let reply = (!replies.is_empty()).then_some(Value::Array(replies));
                    self.answer(reply, act).await;
                    return;
                }
                _ => {}
            }
        }
        // ADR-007 Q13/Q14: with the in-flight count full this waits, so the connection is not read.
        let Ok(permit) = self.conn.in_flight.clone().acquire_owned().await else {
            return;
        };
        let (rpc, conn, out) = (self.rpc.clone(), self.conn.clone(), self.out.clone());
        self.tasks.spawn(async move {
            let r = rpc.serve(&conn, text.into_bytes()).await;
            drop(permit);
            if let Some(b) = r.body {
                let _ = out.send(String::from_utf8_lossy(&b).into_owned()).await;
            }
        });
    }

    /// Closing the connection cancels what runs (the `JoinSet` drops) and its subscriptions.
    fn close(self) {
        self.rpc.hub.drop_conn(self.conn.id);
    }
}

pub async fn serve_ws(rpc: Arc<Rpc>, listener: TcpListener) {
    let conns = Arc::new(Semaphore::new(rpc.limits.ws_connections));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let Ok(slot) = conns.clone().try_acquire_owned() else {
            continue;
        };
        let rpc = rpc.clone();
        tokio::spawn(async move {
            // geth `wsHandshakeValidator`: a refused origin never gets a WebSocket.
            #[allow(
                clippy::result_large_err,
                reason = "the error type is tungstenite's callback contract"
            )]
            // With `--rpc.jwtsecret` the upgrade itself carries the token; a refused upgrade gets no
            // WebSocket, so no later frame on it is ever read.
            let check = |req: &WsRequest, resp: WsResponse| {
                let hdr = |n: &str| req.headers().get(n).and_then(|o| o.to_str().ok());
                let refused = match rpc.policy.check_origin(hdr("origin")) {
                    Err(e) => Some((StatusCode::FORBIDDEN, e)),
                    Ok(()) => rpc
                        .policy
                        .check_auth(hdr("authorization"))
                        .err()
                        .map(|e| (StatusCode::UNAUTHORIZED, e)),
                };
                match refused {
                    None => Ok(resp),
                    Some((status, e)) => {
                        let mut r = ErrorResponse::new(Some(e.to_string()));
                        *r.status_mut() = status;
                        Err(r)
                    }
                }
            };
            // #16: a message or frame is bounded like an HTTP body (tungstenite's own default is 64 MiB).
            let cfg = WebSocketConfig::default()
                .max_message_size(Some(rpc.limits.body_bytes))
                .max_frame_size(Some(rpc.limits.body_bytes));
            let Ok(ws) =
                tokio_tungstenite::accept_hdr_async_with_config(stream, check, Some(cfg)).await
            else {
                return;
            };
            let (mut sink, mut stream) = ws.split();
            let (mut s, mut rx) = Session::new(&rpc, rpc.limits.ws_in_flight);
            let writer = tokio::spawn(async move {
                while let Some(m) = rx.recv().await {
                    if sink.send(Message::text(m)).await.is_err() {
                        break;
                    }
                }
            });
            while let Some(Ok(m)) = stream.next().await {
                match m {
                    Message::Text(t) => s.on_message(t.as_str().to_owned()).await,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            s.close();
            writer.abort();
            drop(slot);
        });
    }
}

/// IPC: a Unix socket carrying a stream of JSON values, no framing (geth `rpc/ipc.go`).
pub async fn serve_ipc(rpc: Arc<Rpc>, path: &Path) -> std::io::Result<()> {
    let listener = UnixListener::bind(path)?;
    // geth `rpc/ipc_unix.go`: the socket is the owner's only (0600); IPC serves every namespace.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let conns = Arc::new(Semaphore::new(rpc.limits.ipc_connections));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(slot) = conns.clone().try_acquire_owned() else {
            continue;
        };
        let rpc = rpc.clone();
        tokio::spawn(async move {
            let (mut rd, mut wr) = stream.into_split();
            let (mut s, mut rx) = Session::new(&rpc, rpc.limits.ipc_in_flight);
            let writer = tokio::spawn(async move {
                while let Some(m) = rx.recv().await {
                    if wr.write_all(m.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
            let (mut buf, mut chunk) = (Vec::new(), vec![0u8; 8192]);
            'conn: loop {
                match rd.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                let mut used = 0;
                let mut de = serde_json::Deserializer::from_slice(&buf).into_iter::<Value>();
                while let Some(v) = de.next() {
                    match v {
                        Ok(v) => {
                            used = de.byte_offset();
                            s.on_message(v.to_string()).await;
                        }
                        Err(e) if e.is_eof() => break,
                        Err(_) => break 'conn,
                    }
                }
                buf.drain(..used);
                if buf.len() > rpc.limits.body_bytes {
                    break;
                }
            }
            s.close();
            writer.abort();
            drop(slot);
        });
    }
}
