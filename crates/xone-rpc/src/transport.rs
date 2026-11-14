//! HTTP (hyper), WebSocket (tokio-tungstenite) and IPC (a Unix socket carrying a JSON stream).
//! Each listener bounds its connections (ADR-007 Q12-Q14), and every request goes through
//! [`Rpc::serve`], so the quotas, deadlines and cancellation are the same on all three. A request
//! whose connection closes is cancelled: its future is dropped and the cancel flag is set.

use crate::{
    server::{Conn, Rpc},
    subscriptions::parse_kind,
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

/// Responses shorter than this go out plain: gzip framing would cost more than it saves.
pub const GZIP_MIN_BYTES: usize = 1024;
/// Above this the compression runs on a blocking thread, off the runtime.
const GZIP_INLINE_MAX: usize = 64 << 10;

/// Whether an `Accept-Encoding` value admits gzip (RFC 9110 12.5.3): `gzip` or `*` with a
/// non-zero quality; an explicit `gzip;q=0` refuses it, whatever `*` says.
pub fn accepts_gzip(header: Option<&str>) -> bool {
    let Some(h) = header else {
        return false;
    };
    let (mut gzip, mut star) = (None, None);
    for item in h.split(',') {
        let mut parts = item.split(';');
        let name = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let mut q = true;
        for p in parts {
            if let Some((k, v)) = p.split_once('=') {
                if k.trim().eq_ignore_ascii_case("q") {
                    q = v.trim().parse::<f64>().is_ok_and(|x| x > 0.0);
                }
            }
        }
        match name.as_str() {
            "gzip" | "x-gzip" => gzip = Some(q),
            "*" => star = Some(q),
            _ => {}
        }
    }
    gzip.or(star).unwrap_or(false)
}

fn gzip(body: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut z = flate2::write::GzEncoder::new(
        Vec::with_capacity(body.len() / 4),
        flate2::Compression::fast(),
    );
    z.write_all(body).ok()?;
    z.finish().ok()
}

/// The body to send and whether it is gzip-encoded. The response limits apply before this, to the
/// JSON itself; compression only shrinks it.
async fn encode(body: Vec<u8>, gz: bool) -> (Vec<u8>, bool) {
    if !gz || body.len() < GZIP_MIN_BYTES {
        return (body, false);
    }
    let z = if body.len() <= GZIP_INLINE_MAX {
        gzip(&body)
    } else {
        let b = body.clone();
        tokio::task::spawn_blocking(move || gzip(&b))
            .await
            .unwrap_or(None)
    };
    match z {
        Some(z) => (z, true),
        None => (body, false),
    }
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

/// geth `acceptedContentTypes`: the media type, without its parameters, compared case-blind.
/// A missing or malformed header is refused, as geth's `mime.ParseMediaType` error is.
fn json_media_type(ct: Option<&str>) -> bool {
    let Some(ct) = ct else {
        return false;
    };
    let mt = ct.split(';').next().unwrap_or("").trim();
    [
        "application/json",
        "application/json-rpc",
        "application/jsonrequest",
    ]
    .iter()
    .any(|a| mt.eq_ignore_ascii_case(a))
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
    // geth `rpc/http.go` validateRequest: the media type must be a JSON one, before any body is
    // read or a method runs. Simple form and text types (a browser's no-preflight POST) fail.
    let ct = req
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok());
    if !json_media_type(ct) {
        return Ok(http_reply(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            b"invalid content type, only application/json is supported".to_vec(),
            None,
        ));
    }
    let gz = accepts_gzip(
        req.headers()
            .get("accept-encoding")
            .and_then(|h| h.to_str().ok()),
    );
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
    let (body, zipped) = encode(reply.body.unwrap_or_else(Vec::new), gz).await;
    let mut r = http_reply(status, body, reply.retry_after);
    // the representation depends on Accept-Encoding: caches must key on it
    r.headers_mut().insert(
        "vary",
        hyper::header::HeaderValue::from_static("Accept-Encoding"),
    );
    if zipped {
        r.headers_mut().insert(
            "content-encoding",
            hyper::header::HeaderValue::from_static("gzip"),
        );
    }
    Ok(r)
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

    /// `eth_subscribe` and `eth_unsubscribe`, answered inline. `None` for any other message.
    fn subscription(&self, msg: &Value) -> Option<Value> {
        let method = msg.get("method")?.as_str()?;
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let params: &[Value] = msg
            .get("params")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice);
        let res: Result<Value, RpcError> = match method {
            "eth_subscribe" => parse_kind(params)
                .map(|k| json!(self.rpc.hub.subscribe(self.conn.id, k, self.out.clone()))),
            "eth_unsubscribe" => Ok(json!(params
                .first()
                .and_then(Value::as_str)
                .is_some_and(|s| self.rpc.hub.unsubscribe(self.conn.id, s)))),
            _ => return None,
        };
        Some(match res {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e.to_value()}),
        })
    }

    async fn on_message(&mut self, text: String) {
        while self.tasks.try_join_next().is_some() {}
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if let Some(reply) = self.subscription(&v) {
                let _ = self.out.send(reply.to_string()).await;
                return;
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

/// Binds the IPC socket at `path`, owner-only (0600) from the moment it is reachable there.
///
/// geth `rpc/ipc_unix.go`: the socket is the owner's only; IPC serves every namespace. The socket
/// is bound inside a fresh 0700 directory next to `path`, set to 0600 and then renamed into place,
/// so there is no window in which another user can connect between bind and chmod. A stale socket
/// left by a killed node is replaced; a live one (something answers on it) or a path that is not a
/// socket is refused, and nothing is removed. Must run inside a Tokio runtime.
pub fn bind_ipc(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
    use std::{fs, io};
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("{}: another process is listening", path.display()),
                ));
            }
            fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{}: exists and is not a socket", path.display()),
            ))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let parent = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "IPC path has no file name"))?;
    let tmp = parent.join(format!(
        ".{}.{}.bind",
        name.to_string_lossy(),
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);
    fs::DirBuilder::new().mode(0o700).create(&tmp)?;
    let inner = tmp.join("s");
    let bound = (|| {
        let l = UnixListener::bind(&inner)?;
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o600))?;
        fs::rename(&inner, path)?;
        Ok(l)
    })();
    let _ = fs::remove_dir_all(&tmp);
    bound
}

/// IPC: a Unix socket carrying a stream of JSON values, no framing (geth `rpc/ipc.go`).
pub async fn serve_ipc(rpc: Arc<Rpc>, path: &Path) -> std::io::Result<()> {
    serve_ipc_on(rpc, bind_ipc(path)?).await
}

/// [`serve_ipc`] on a listener from [`bind_ipc`], so the caller reports a bind failure at start.
pub async fn serve_ipc_on(rpc: Arc<Rpc>, listener: UnixListener) -> std::io::Result<()> {
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

#[cfg(test)]
mod gzip_tests {
    use super::*;

    #[test]
    fn accept_encoding_is_negotiated_as_rfc_9110_says() {
        assert!(!accepts_gzip(None));
        assert!(!accepts_gzip(Some("")));
        assert!(!accepts_gzip(Some("identity, br")));
        assert!(accepts_gzip(Some("gzip")));
        assert!(accepts_gzip(Some("deflate, GZIP;q=0.5")));
        assert!(accepts_gzip(Some("*")));
        assert!(!accepts_gzip(Some("gzip;q=0")));
        assert!(!accepts_gzip(Some("*, gzip;q=0")));
        assert!(!accepts_gzip(Some("gzip;q=0.000, *;q=1")));
        assert!(!accepts_gzip(Some("*;q=0")));
    }

    #[tokio::test]
    async fn small_or_refused_bodies_go_out_plain() {
        let small = vec![b'x'; GZIP_MIN_BYTES - 1];
        assert_eq!(encode(small.clone(), true).await, (small, false));
        let big = vec![b'x'; 200 << 10];
        assert_eq!(encode(big.clone(), false).await, (big.clone(), false));
        let (z, zipped) = encode(big.clone(), true).await;
        assert!(zipped && z.len() < big.len() / 10);
        let mut out = vec![];
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&z[..]), &mut out).unwrap();
        assert_eq!(out, big);
    }
}
