//! ethstats client (XB-21). Wire format of geth-XDC `ethstats/ethstats.go` at the pin
//! (see spec/ops/ethstats.md): `{"emit":[event,payload]}` text frames, newline-terminated like
//! gorilla's `WriteJSON`, hello then ready, then `node-ping`/`latency`, `block`, `pending`,
//! `stats` and `history`. Key order is the Go struct order, or sorted for Go maps.

use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering::SeqCst},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tungstenite::{Message, WebSocket};

use super::{hex0x, wire, BlockInfo, ChainStats, PeerStats};
use crate::{
    lifecycle::{Service, ServiceError},
    TokenBucket,
};

const HISTORY_MAX: usize = 50; // geth historyUpdateRange
const FULL_REPORT: Duration = Duration::from_secs(15); // geth fullReport ticker
const TICK: Duration = Duration::from_millis(500);

/// Static facts of the hello frame. `node` and `client` carry the one build stamp.
#[derive(Clone, Debug)]
pub struct Identity {
    pub node: String,
    pub client: String,
    pub port: u16,
    pub net: String,
    pub protocol: String,
    pub os: String,
    pub os_v: String,
    pub sync_mode: String,
    pub gc_mode: String,
    pub state_scheme: String,
}

#[derive(Clone)]
pub struct EthstatsCfg {
    /// `name:secret@host[:port]`, host optionally `ws://` or `wss://` prefixed. The secret
    /// comes from the env file, never a default.
    pub url: String,
    /// Private devnets only. Without it a `ws://` URL is refused.
    pub allow_plaintext: bool,
    pub identity: Identity,
}

impl fmt::Debug for EthstatsCfg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The URL holds the secret: never printed.
        f.debug_struct("EthstatsCfg")
            .field("url", &"<redacted>")
            .field("allow_plaintext", &self.allow_plaintext)
            .field("identity", &self.identity)
            .finish()
    }
}

struct Parsed {
    name: String,
    secret: String,
    target: wire::Target,
}

fn parse(url: &str) -> Result<Parsed, String> {
    // geth: last '@' splits the host, last ':' of the left part splits the secret.
    let (pre, host) = url.rsplit_once('@').ok_or("missing '@' in ethstats URL")?;
    let (name, secret) = pre.rsplit_once(':').unwrap_or((pre, ""));
    if name.is_empty() {
        return Err("empty ethstats node name".into());
    }
    Ok(Parsed {
        name: name.to_owned(),
        secret: secret.to_owned(),
        target: wire::parse_target(host, ["wss", "ws"], "/api")?,
    })
}

// ---- frames: pure, byte-compared with the recorded geth output ----

/// JSON string as Go's encoding/json writes it (it escapes <, > and & too).
fn js(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_default()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

fn emit(event: &str, payload: &str) -> String {
    format!("{{\"emit\":[\"{event}\",{payload}]}}")
}

pub fn hello(id: &str, secret: &str, i: &Identity, signer: Option<[u8; 20]>) -> String {
    // Zero address and "follower" when not sealing: the board matches a full 40-hex address.
    let (coinbase, role) = match signer.filter(|a| *a != [0u8; 20]) {
        Some(a) => (hex0x(&a), "validator"),
        None => (hex0x(&[0u8; 20]), "follower"),
    };
    emit(
        "hello",
        &format!(
            "{{\"id\":{id},\"info\":{{\"name\":{id},\"node\":{node},\"port\":{port},\"net\":{net},\
\"protocol\":{proto},\"api\":\"No\",\"os\":{os},\"os_v\":{osv},\"client\":{client},\
\"canUpdateHistory\":true,\"coinbase\":\"{coinbase}\",\"role\":\"{role}\",\"syncMode\":{sm},\
\"gcMode\":{gm},\"stateScheme\":{ss}}},\"secret\":{secret}}}",
            id = js(id),
            node = js(&i.node),
            port = i.port,
            net = js(&i.net),
            proto = js(&i.protocol),
            os = js(&i.os),
            osv = js(&i.os_v),
            client = js(&i.client),
            sm = js(&i.sync_mode),
            gm = js(&i.gc_mode),
            ss = js(&i.state_scheme),
            secret = js(secret),
        ),
    )
}

pub fn node_ping(id: &str, client_time: &str) -> String {
    emit(
        "node-ping",
        &format!("{{\"clientTime\":{},\"id\":{}}}", js(client_time), js(id)),
    )
}

pub fn latency(id: &str, ms: u64) -> String {
    emit(
        "latency",
        &format!("{{\"id\":{},\"latency\":\"{ms}\"}}", js(id)),
    )
}

fn block_json(b: &BlockInfo) -> String {
    let txs: Vec<String> = b
        .tx_hashes
        .iter()
        .map(|h| format!("{{\"hash\":\"{}\"}}", hex0x(h)))
        .collect();
    format!(
        "{{\"number\":{},\"hash\":\"{}\",\"parentHash\":\"{}\",\"timestamp\":{},\"miner\":\"{}\",\
\"gasUsed\":{},\"gasLimit\":{},\"difficulty\":\"{}\",\"totalDifficulty\":\"0\",\
\"transactions\":[{}],\"transactionsRoot\":\"{}\",\"stateRoot\":\"{}\",\"uncles\":[]}}",
        b.number,
        hex0x(&b.hash),
        hex0x(&b.parent_hash),
        b.timestamp,
        hex0x(&b.miner),
        b.gas_used,
        b.gas_limit,
        b.difficulty,
        txs.join(","),
        hex0x(&b.tx_root),
        hex0x(&b.state_root),
    )
}

pub fn block(id: &str, b: &BlockInfo) -> String {
    emit(
        "block",
        &format!("{{\"block\":{},\"id\":{}}}", block_json(b), js(id)),
    )
}

pub fn pending(id: &str, n: usize) -> String {
    emit(
        "pending",
        &format!("{{\"id\":{},\"stats\":{{\"pending\":{n}}}}}", js(id)),
    )
}

pub fn stats(id: &str, syncing: bool, peers: usize, gas_price: u64) -> String {
    emit(
        "stats",
        &format!(
            "{{\"id\":{},\"stats\":{{\"active\":true,\"syncing\":{syncing},\"peers\":{peers},\
\"gasPrice\":{gas_price},\"uptime\":100}}}}",
            js(id)
        ),
    )
}

/// geth `reportHistory`: the reply lists the found blocks newest first and stops at the first
/// unknown number.
pub fn history(id: &str, blocks_in_request_order: &[BlockInfo]) -> String {
    let b: Vec<String> = blocks_in_request_order
        .iter()
        .rev()
        .map(block_json)
        .collect();
    emit(
        "history",
        &format!("{{\"history\":[{}],\"id\":{}}}", b.join(","), js(id)),
    )
}

/// Go `time.Time.String()` without the monotonic suffix, UTC.
fn go_time(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (secs, nanos) = (d.as_secs(), d.subsec_nanos());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Hinnant).
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{nanos:09} +0000 UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

// ---- session ----

type Ws = WebSocket<Box<dyn wire::Io>>;

fn send(ws: &mut Ws, frame: &str) -> Result<(), String> {
    // gorilla's WriteJSON ends the message with the encoder's newline.
    ws.send(Message::text(format!("{frame}\n")))
        .map_err(|e| e.to_string())
}

fn is_idle(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io)
        if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

struct Session<'a> {
    id: &'a str,
    chain: &'a dyn ChainStats,
    peers: &'a dyn PeerStats,
    ping_at: Option<Instant>,
}

impl Session<'_> {
    fn report_block(&self, ws: &mut Ws) -> Result<(), String> {
        match self.chain.head() {
            Some(b) => send(ws, &block(self.id, &b)),
            None => Ok(()),
        }
    }
    fn report_pending(&self, ws: &mut Ws) -> Result<(), String> {
        send(ws, &pending(self.id, self.chain.pending_txs()))
    }
    fn report_stats(&self, ws: &mut Ws) -> Result<(), String> {
        send(
            ws,
            &stats(
                self.id,
                self.chain.syncing(),
                self.peers.peer_count(),
                self.chain.gas_price(),
            ),
        )
    }
    /// geth `report`: latency ping, block, pending, stats.
    fn report(&mut self, ws: &mut Ws) -> Result<(), String> {
        send(ws, &node_ping(self.id, &go_time(SystemTime::now())))?;
        self.ping_at = Some(Instant::now());
        self.report_block(ws)?;
        self.report_pending(ws)?;
        self.report_stats(ws)
    }

    fn history(&self, ws: &mut Ws, list: Vec<u64>) -> Result<(), String> {
        let list = if list.is_empty() {
            let head = self.chain.head().map_or(0, |b| b.number);
            let start = (head + 1).saturating_sub(HISTORY_MAX as u64);
            (start..=head).collect()
        } else {
            list
        };
        let mut found = vec![];
        for n in list {
            match self.chain.block(n) {
                Some(b) => found.push(b),
                None => break,
            }
        }
        send(ws, &history(self.id, &found))
    }

    fn on_text(&mut self, ws: &mut Ws, text: &str) -> Result<(), String> {
        if let Some(ts) = text.trim().strip_prefix("\"primus::ping::") {
            // A bare JSON string; the pong echoes it.
            let pong = format!("\"primus::pong::{ts}");
            return ws.send(Message::text(pong)).map_err(|e| e.to_string());
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
            return Ok(());
        };
        let Some(emit) = v.get("emit").and_then(|e| e.as_array()) else {
            return Ok(());
        };
        match emit.first().and_then(|e| e.as_str()) {
            Some("node-pong") => {
                if let Some(t) = self.ping_at.take() {
                    let ms = u64::try_from(t.elapsed().as_millis() / 2).unwrap_or(u64::MAX);
                    send(ws, &latency(self.id, ms))?;
                }
            }
            Some("history") => {
                let list = emit
                    .get(1)
                    .and_then(|r| r.get("list"))
                    .and_then(|l| l.as_array())
                    .map(|l| l.iter().filter_map(|n| n.as_u64()).collect())
                    .unwrap_or_default();
                self.history(ws, list)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn session(
    p: &Parsed,
    cfg: &EthstatsCfg,
    chain: &dyn ChainStats,
    peers: &dyn PeerStats,
    stop: &AtomicBool,
) -> Result<bool, String> {
    let (stream, ctl) = wire::connect(&p.target, cfg.allow_plaintext, Duration::from_secs(3))
        .map_err(|e| e.to_string())?;
    let req = format!(
        "{}://{}:{}{}",
        if p.target.tls { "wss" } else { "ws" },
        p.target.url_host(),
        p.target.port,
        p.target.path
    );
    let (mut ws, _) =
        tungstenite::client(req.as_str(), stream).map_err(|e| format!("handshake: {e}"))?;
    send(
        &mut ws,
        &hello(&p.name, &p.secret, &cfg.identity, chain.signer()),
    )?;
    // Wait for ready; only a bare ping may come first.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() > deadline || stop.load(SeqCst) {
            return Err("no ready from server".into());
        }
        match ws.read() {
            Ok(Message::Text(t)) => {
                if t.trim().starts_with("\"primus::ping::") {
                    let pong = t.trim().replacen("primus::ping::", "primus::pong::", 1);
                    ws.send(Message::text(pong)).map_err(|e| e.to_string())?;
                    continue;
                }
                let v: serde_json::Value = serde_json::from_str(&t).map_err(|_| "bad ack")?;
                if v == serde_json::json!({"emit": ["ready"]}) {
                    break;
                }
                return Err("unauthorized".into());
            }
            Ok(_) => {}
            Err(e) if is_idle(&e) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    ctl.set_read_timeout(Some(TICK))
        .map_err(|e| e.to_string())?;
    let mut s = Session {
        id: &p.name,
        chain,
        peers,
        ping_at: None,
    };
    s.report(&mut ws)?;
    let mut last_hash = chain.head().map(|b| b.hash);
    let mut last_full = Instant::now();
    let t0 = Instant::now();
    // Head reports are bounded: 10 burst, 5 per second, whatever the chain does.
    let mut bucket = TokenBucket::new(10, 5, 0);
    while !stop.load(SeqCst) {
        match ws.read() {
            Ok(Message::Text(t)) => s.on_text(&mut ws, &t)?,
            Ok(Message::Close(_)) => return Ok(true),
            Ok(_) => {}
            Err(e) if is_idle(&e) => {}
            Err(e) => return Err(e.to_string()),
        }
        let now_ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
        let head = chain.head().map(|b| b.hash);
        if head != last_hash && bucket.try_take(now_ms, 1) {
            last_hash = head;
            s.report_block(&mut ws)?;
            s.report_pending(&mut ws)?;
        }
        if last_full.elapsed() >= FULL_REPORT {
            last_full = Instant::now();
            s.report(&mut ws)?;
        }
    }
    let _ = ws.close(None);
    Ok(true)
}

pub struct Ethstats {
    cfg: EthstatsCfg,
    chain: Arc<dyn ChainStats>,
    peers: Arc<dyn PeerStats>,
    stop: Arc<AtomicBool>,
    th: Option<JoinHandle<()>>,
}

impl Ethstats {
    pub fn new(cfg: EthstatsCfg, chain: Arc<dyn ChainStats>, peers: Arc<dyn PeerStats>) -> Self {
        Self {
            cfg,
            chain,
            peers,
            stop: Arc::default(),
            th: None,
        }
    }
}

fn nap(stop: &AtomicBool, d: Duration) {
    let end = Instant::now() + d;
    while !stop.load(SeqCst) && Instant::now() < end {
        thread::sleep(Duration::from_millis(100));
    }
}

impl Service for Ethstats {
    fn name(&self) -> &str {
        "ethstats"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let p = parse(&self.cfg.url).map_err(ServiceError)?;
        if !p.target.tls && !self.cfg.allow_plaintext {
            return Err(ServiceError(
                "plaintext ethstats refused: use wss or allow_plaintext on a private devnet".into(),
            ));
        }
        let (cfg, chain, peers, stop) = (
            self.cfg.clone(),
            self.chain.clone(),
            self.peers.clone(),
            self.stop.clone(),
        );
        let th = thread::Builder::new()
            .name("ethstats".into())
            .spawn(move || {
                let mut backoff = Duration::from_secs(1);
                while !stop.load(SeqCst) {
                    let began = Instant::now();
                    match session(&p, &cfg, &*chain, &*peers, &stop) {
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(target: "xone_node::ethstats", "session ended: {e}")
                        }
                    }
                    // A session that lasted resets the wait; quick failures back off to 60 s.
                    backoff = if began.elapsed() > Duration::from_secs(30) {
                        Duration::from_secs(1)
                    } else {
                        (backoff * 2).min(Duration::from_secs(60))
                    };
                    nap(&stop, backoff);
                }
            })
            .map_err(|e| ServiceError(e.to_string()))?;
        self.th = Some(th);
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(t) = self.th.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::testutil::{block as tblock, Fake};
    use std::{net::TcpListener, sync::Mutex};

    fn golden() -> Vec<(String, String)> {
        include_str!("../../tests/fixtures/ethstats/golden.tsv")
            .lines()
            .map(|l| {
                let (n, f) = l.split_once('\t').unwrap();
                (n.to_owned(), f.to_owned())
            })
            .collect()
    }
    fn g(name: &str) -> String {
        golden().into_iter().find(|(n, _)| n == name).unwrap().1
    }
    fn ident() -> Identity {
        Identity {
            node: "xone/v0.1.0-abc1234".into(),
            client: "xone/v0.1.0-abc1234".into(),
            port: 30303,
            net: "51".into(),
            protocol: "eth/63, xdpos/100".into(),
            os: "linux".into(),
            os_v: "amd64".into(),
            sync_mode: "full".into(),
            gc_mode: "full".into(),
            state_scheme: "hash".into(),
        }
    }
    const ADDR: [u8; 20] = [
        0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67,
        0x89, 0xab, 0xcd, 0xef, 0x01,
    ];

    /// Byte for byte against frames produced by the pinned geth marshalling
    /// (tests/fixtures/ethstats/golden_gen.go.txt).
    #[test]
    fn frames_equal_recorded_geth_frames() {
        let id = "xone-test";
        assert_eq!(hello(id, "s3cret", &ident(), Some(ADDR)), g("hello"));
        assert_eq!(
            node_ping(id, "2026-11-11 10:00:00.123456789 +0000 UTC"),
            g("node-ping")
        );
        assert_eq!(latency(id, 12), g("latency"));
        assert_eq!(block(id, &tblock(12345, 2)), g("block"));
        assert_eq!(block(id, &tblock(7, 0)), g("block_empty"));
        assert_eq!(pending(id, 5), g("pending"));
        assert_eq!(stats(id, false, 9, 250_000_000), g("stats"));
        assert_eq!(history(id, &[tblock(1, 1), tblock(2, 0)]), g("history"));
    }

    #[test]
    fn follower_hello_has_zero_coinbase() {
        let h = hello("n", "", &ident(), None);
        assert!(h.contains(
            "\"coinbase\":\"0x0000000000000000000000000000000000000000\",\"role\":\"follower\""
        ));
        assert_eq!(hello("n", "", &ident(), Some([0; 20])), h);
    }

    #[test]
    fn go_time_format() {
        assert_eq!(
            go_time(UNIX_EPOCH + Duration::new(1_794_391_200, 123_456_789)),
            "2026-11-11 10:00:00.123456789 +0000 UTC"
        );
    }

    #[test]
    fn url_and_secret_handling() {
        let p = parse("n:p:w@host.example:3000").unwrap();
        assert_eq!((p.name.as_str(), p.secret.as_str()), ("n:p", "w")); // geth: last ':' of the left part
        assert!(parse("nohost").is_err());
        let cfg = EthstatsCfg {
            url: "n:topsecret@h".into(),
            allow_plaintext: false,
            identity: ident(),
        };
        assert!(!format!("{cfg:?}").contains("topsecret"));
        let mut svc = Ethstats::new(
            EthstatsCfg {
                url: "n:topsecret@ws://127.0.0.1:1".into(),
                ..cfg
            },
            Arc::new(Fake::new(1)),
            Arc::new(Fake::new(1)),
        );
        let e = svc.start().unwrap_err();
        assert!(!e.0.contains("topsecret"), "{}", e.0);
    }

    /// Fake server: accepts one client, records frames, answers hello with ready and node-ping
    /// with node-pong, and sends a primus ping first.
    #[test]
    fn session_against_fake_server() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        let srv = thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut ws = tungstenite::accept(s).unwrap();
            loop {
                let m = match ws.read() {
                    Ok(m) => m,
                    Err(_) => return,
                };
                let Message::Text(t) = m else { continue };
                let t = t.to_string();
                if t.contains("\"hello\"") {
                    ws.send(Message::text("\"primus::ping::1\"")).unwrap();
                    ws.send(Message::text("{\"emit\":[\"ready\"]}")).unwrap();
                } else if t.contains("\"node-ping\"") {
                    ws.send(Message::text("{\"emit\":[\"node-pong\",{}]}"))
                        .unwrap();
                    ws.send(Message::text("{\"emit\":[\"history\",{\"list\":[1,2]}]}"))
                        .unwrap();
                }
                seen2.lock().unwrap().push(t);
            }
        });
        let chain = Arc::new(Fake {
            signer: Some(ADDR),
            ..Fake::new(12345)
        });
        let cfg = EthstatsCfg {
            url: format!("xone-test:s3cret@ws://127.0.0.1:{port}"),
            allow_plaintext: true,
            identity: ident(),
        };
        let mut svc = Ethstats::new(cfg, chain.clone(), chain.clone());
        svc.start().unwrap();
        let t0 = Instant::now();
        while seen.lock().unwrap().len() < 6 && t0.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(20));
        }
        // A new head is reported without waiting for the 15 s timer.
        chain.head.store(12346, SeqCst);
        chain.salt.store(1, SeqCst);
        while !seen
            .lock()
            .unwrap()
            .iter()
            .any(|f| f.contains("\"number\":12346"))
            && t0.elapsed() < Duration::from_secs(8)
        {
            thread::sleep(Duration::from_millis(20));
        }
        svc.stop();
        drop(srv);
        let f = seen.lock().unwrap().clone();
        assert_eq!(f[0], format!("{}\n", g("hello")));
        assert!(f[1].starts_with("\"primus::pong::1"), "{}", f[1]);
        assert!(f[2].starts_with("{\"emit\":[\"node-ping\",{\"clientTime\":\""));
        assert_eq!(f[3], format!("{}\n", g("block")));
        assert_eq!(f[4], format!("{}\n", g("pending")));
        assert_eq!(f[5], format!("{}\n", g("stats")));
        assert!(f
            .iter()
            .any(|x| x.starts_with("{\"emit\":[\"latency\",{\"id\":\"xone-test\",\"latency\":\"")));
        assert!(
            f.iter().any(|x| x.contains("\"history\":[{\"number\":2,")),
            "{f:?}"
        );
        assert!(f.iter().any(|x| x.contains("\"number\":12346")));
    }

    /// A dropped connection is re-dialled after a backoff and logs in again.
    #[test]
    fn reconnects_after_a_drop() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hellos = Arc::new(Mutex::new(0u32));
        let h2 = hellos.clone();
        let srv = thread::spawn(move || {
            for round in 0..2 {
                let Ok((s, _)) = l.accept() else { return };
                let mut ws = tungstenite::accept(s).unwrap();
                if ws.read().is_ok() {
                    *h2.lock().unwrap() += 1;
                }
                if round == 0 {
                    continue; // drop without ready
                }
                ws.send(Message::text("{\"emit\":[\"ready\"]}")).unwrap();
                while ws.read().is_ok() {}
            }
        });
        let f = Arc::new(Fake::new(1));
        let cfg = EthstatsCfg {
            url: format!("n:s@ws://127.0.0.1:{port}"),
            allow_plaintext: true,
            identity: ident(),
        };
        let mut svc = Ethstats::new(cfg, f.clone(), f);
        svc.start().unwrap();
        let t0 = Instant::now();
        while *hellos.lock().unwrap() < 2 && t0.elapsed() < Duration::from_secs(8) {
            thread::sleep(Duration::from_millis(50));
        }
        svc.stop();
        assert_eq!(*hellos.lock().unwrap(), 2);
        drop(srv);
    }
}
