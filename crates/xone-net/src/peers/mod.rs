//! Peer sessions: bounded dial and handshake, admission, per-peer queues with the bounds of
//! ADR-007 section 2, scoring, BFT gossip and a `shutdown` with a time bound.
//!
//! One task per peer reads frames and forwards messages to the application through a bounded
//! channel of [`Event`]s. A session never stops reading the socket to wait for the application:
//! a full per-peer queue drops, rejects or disconnects according to its policy (ADR-007
//! section 2: no backpressure on a peer socket).
//!
//! Trust ([`trusted`]) changes who gets a slot and who is banned. It does not change what is
//! validated: every message of every peer is decoded strictly, checked against its size cap and
//! counted against the same queue bounds, and an undecodable data-plane message ends the
//! session of a trusted peer as it ends any other.

pub mod score;
pub mod trusted;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use alloy_primitives::{keccak256, B256, U256};
use futures::future::join_all;
use secp256k1::SecretKey;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, timeout, Instant};
use xone_primitives::limits::{BoundedQueue, Offence, Overflow, Push};
use xone_primitives::Header;

use crate::bft_gossip::{Gossip, Receive};
use crate::eth::status62::Local;
use crate::eth::{self, codes, BftMessage, Framing, Message, Status62};
use crate::rlpx::{peer_id, Cap, Conn, DisconnectReason, LocalHello, PeerId, Sub};
use crate::NetError;
use score::{Scoreboard, Verdict};
use trusted::{admit, Admission, Trust};

pub use xone_primitives::limits::PeerLimits;

/// Timeouts and limits. Every value is a bound: there is no "wait forever" setting.
#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub network_id: u64,
    pub genesis: B256,
    /// Head and TD we put in our Status (TD only ranks us on the other side).
    pub head: B256,
    pub td: U256,
    /// eth versions we offer, best first. Default `[101, 100]`; `[100]` is for talking to (or
    /// testing against) a legacy-only peer. Only 100 and 101 exist here.
    pub versions: Vec<u64>,
    /// Capabilities offered next to eth. Default: all of [`Sub::ALL`] (every node serves snap/1
    /// and xpre/1 from its retained state window).
    pub subs: Vec<Sub>,
    /// ADR-007 section 8: 50.
    pub max_peers: usize,
    /// TCP connect. xOneGo `b437ca9a`: 1 to 5 s so a dial cannot hold up shutdown.
    pub dial_timeout: Duration,
    /// ECIES, Hello and Status together (geth `p2p/transport.go:39`, `handshake.go:77`: 5 s).
    pub handshake_timeout: Duration,
    /// Time allowed for one Disconnect frame.
    pub disconnect_timeout: Duration,
    /// Time allowed for one message write; a peer that stops reading is dropped
    /// (geth `p2p/server.go:57` read timeout is 30 s, write 20 s).
    pub write_timeout: Duration,
    /// Silence after which a peer is dropped. A live peer pings every 15 s (`p2p/peer.go:48`).
    pub idle_timeout: Duration,
    /// Time [`Peers::shutdown`] waits for sessions before aborting them.
    pub shutdown_grace: Duration,
    /// Handshakes in progress at once; more are refused (unauthenticated sockets are cheap).
    pub max_handshakes: usize,
    /// Messages queued for one peer to send.
    pub outbound_queue: usize,
    /// ADR-007 Q2: events waiting for the application.
    pub event_queue: usize,
    /// `--eth.requiredblocks`: a peer that serves another hash at one of these heights is dropped
    /// (geth `eth/handler.go`). A peer that does not have the height yet is kept.
    pub required_blocks: Vec<(u64, B256)>,
    /// Time one required-block header may take (geth: 15 s).
    pub required_timeout: Duration,
    pub limits: PeerLimits,
}

impl Config {
    pub fn new(name: &str, network_id: u64, genesis: B256) -> Self {
        Self {
            name: name.to_owned(),
            network_id,
            genesis,
            head: genesis,
            td: U256::ZERO,
            versions: vec![eth::v101::VERSION, eth::v100::VERSION],
            subs: Sub::ALL.to_vec(),
            max_peers: 50,
            dial_timeout: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(5),
            disconnect_timeout: Duration::from_secs(1),
            write_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(2),
            max_handshakes: 64,
            outbound_queue: 64,
            event_queue: 4096,
            required_blocks: Vec::new(),
            required_timeout: Duration::from_secs(15),
            limits: PeerLimits::default(),
        }
    }
}

/// What the application sees.
#[derive(Debug)]
pub enum Event {
    Connected {
        peer: PeerId,
        version: u64,
        /// The peer's TD, saturated. Ranking only.
        rank: u64,
        /// The head hash of the peer's Status.
        head: B256,
        inbound: bool,
        /// The capabilities next to eth that both sides offer.
        subs: Vec<Sub>,
    },
    /// Any message except Status. A BFT message arrives once, after dedup; call
    /// [`Peers::relay_bft`] after verifying it.
    Message {
        peer: PeerId,
        msg: Message,
    },
    Disconnected {
        peer: PeerId,
    },
}

#[derive(Debug)]
enum Cmd {
    Send(Message),
    Close(DisconnectReason),
}

/// Result of [`Peers::shutdown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Sessions that ended within the grace time.
    pub closed: usize,
    /// Sessions that did not and were aborted.
    pub aborted: usize,
}

struct Entry {
    seq: u64,
    tx: mpsc::Sender<Cmd>,
    task: JoinHandle<()>,
}

struct State {
    closed: bool,
    next_seq: u64,
    head: (B256, U256),
    peers: BTreeMap<PeerId, Entry>,
    gossip: Gossip,
    score: Scoreboard,
    trust: Trust,
}

#[derive(Clone)]
pub struct Peers {
    key: SecretKey,
    id: PeerId,
    cfg: Arc<Config>,
    hello: Arc<LocalHello>,
    state: Arc<Mutex<State>>,
    events: mpsc::Sender<Event>,
    handshakes: Arc<Semaphore>,
    start: Instant,
    /// Peers dropped by the required-block check; `xone_peers_required_block_refused_total`.
    refused: Arc<AtomicU64>,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    // A panic in one session must not take the whole peer set with it.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn framing(version: u64) -> Framing {
    if version == eth::v101::VERSION {
        Framing::RequestId
    } else {
        Framing::Bare
    }
}

async fn bye<Io: AsyncRead + AsyncWrite + Unpin>(
    conn: &mut Conn<Io>,
    reason: DisconnectReason,
    within: Duration,
) {
    // best effort: the peer may be gone or not reading
    let _ = timeout(within, conn.disconnect(reason)).await;
}

impl Peers {
    pub fn new(cfg: Config, key: SecretKey) -> (Self, mpsc::Receiver<Event>) {
        let (events, rx) = mpsc::channel(cfg.event_queue.max(1));
        let state = State {
            closed: false,
            next_seq: 0,
            head: (cfg.head, cfg.td),
            peers: BTreeMap::new(),
            gossip: Gossip::default(),
            score: Scoreboard::new(cfg.limits),
            trust: Trust::default(),
        };
        let hello = LocalHello {
            name: cfg.name.clone(),
            // 101 first, then 100 (`protocol.go:52-61`), then snap/1 and xpre/1
            caps: cfg
                .versions
                .iter()
                .map(|v| Cap::eth(*v))
                .chain(cfg.subs.iter().map(|s| s.cap()))
                .collect(),
        };
        let peers = Self {
            key,
            id: peer_id(&key),
            handshakes: Arc::new(Semaphore::new(cfg.max_handshakes.max(1))),
            cfg: Arc::new(cfg),
            hello: Arc::new(hello),
            state: Arc::new(Mutex::new(state)),
            events,
            start: Instant::now(),
            refused: Arc::new(AtomicU64::new(0)),
        };
        (peers, rx)
    }

    pub fn id(&self) -> PeerId {
        self.id
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Peers dropped for a wrong hash at, or no answer about, a required block.
    pub fn required_refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    pub fn peer_count(&self) -> usize {
        lock(&self.state).peers.len()
    }

    pub fn peer_ids(&self) -> Vec<PeerId> {
        lock(&self.state).peers.keys().copied().collect()
    }

    pub fn add_trusted(&self, id: PeerId) {
        lock(&self.state).trust.add_trusted(id);
    }

    pub fn add_static(&self, id: PeerId) {
        lock(&self.state).trust.add_static(id);
    }

    /// The head we announce in the Status of new sessions.
    pub fn set_head(&self, head: B256, td: U256) {
        lock(&self.state).head = (head, td);
    }

    /// Dial `addr`, expecting node `remote`. Bounded by `dial_timeout` and `handshake_timeout`.
    pub async fn dial(&self, addr: SocketAddr, remote: PeerId) -> Result<PeerId, NetError> {
        if lock(&self.state).closed {
            return Err(NetError::Shutdown);
        }
        let tcp = timeout(self.cfg.dial_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| NetError::Timeout("dial"))??;
        self.attach(tcp, Some(remote)).await
    }

    /// Take an inbound connection (`remote` is `None`) or finish an outbound one: handshake
    /// within `handshake_timeout`, admission, then the session task.
    pub async fn attach<Io>(&self, io: Io, remote: Option<PeerId>) -> Result<PeerId, NetError>
    where
        Io: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let slot = self
            .handshakes
            .try_acquire()
            .map_err(|_| NetError::Rejected(DisconnectReason::TooManyPeers))?;
        let (conn, rank, head) = timeout(self.cfg.handshake_timeout, self.handshake(io, remote))
            .await
            .map_err(|_| NetError::Timeout("handshake"))??;
        drop(slot);
        let id = conn.remote;
        let version = conn.eth_version;
        let subs = conn.subs.clone();
        let inbound = remote.is_none();
        let (tx, rx) = mpsc::channel(self.cfg.outbound_queue.max(1));
        let mut conn = Some(conn);
        let outcome = {
            let mut st = lock(&self.state);
            if st.closed {
                Err(DisconnectReason::Quitting)
            } else {
                let banned = st.score.is_banned(&id, self.now_ms());
                // oldest first: `admit` evicts the newest ordinary peer
                let mut by_age: Vec<(u64, PeerId)> =
                    st.peers.iter().map(|(p, e)| (e.seq, *p)).collect();
                by_age.sort_unstable();
                let connected: Vec<(PeerId, bool)> = by_age
                    .into_iter()
                    .map(|(_, p)| (p, st.trust.is_trusted(&p)))
                    .collect();
                match admit(&st.trust, banned, &connected, self.cfg.max_peers, &id) {
                    Admission::Reject(r) => Err(r),
                    a => {
                        if let Admission::AcceptEvicting(victim) = a {
                            if let Some(v) = st.peers.remove(&victim) {
                                // a full queue drops `tx` right after, which ends the session too
                                let _ = v.tx.try_send(Cmd::Close(DisconnectReason::TooManyPeers));
                            }
                            st.gossip.remove_peer(&victim);
                        }
                        st.next_seq += 1;
                        let seq = st.next_seq;
                        st.gossip.add_peer(id);
                        // Spawned and registered under one lock: the session's first lock waits
                        // for this entry to exist.
                        if let Some(conn) = conn.take() {
                            let task = tokio::spawn(session(
                                conn,
                                self.clone(),
                                Session {
                                    id,
                                    seq,
                                    version,
                                    inbound,
                                    rank,
                                    head,
                                },
                                subs,
                                rx,
                            ));
                            st.peers.insert(id, Entry { seq, tx, task });
                        }
                        Ok(())
                    }
                }
            }
        };
        match (outcome, conn) {
            (Err(r), Some(mut c)) => {
                bye(&mut c, r, self.cfg.disconnect_timeout).await;
                Err(NetError::Rejected(r))
            }
            (Err(r), None) => Err(NetError::Rejected(r)),
            (Ok(()), _) => Ok(id),
        }
    }

    async fn handshake<Io>(
        &self,
        io: Io,
        remote: Option<PeerId>,
    ) -> Result<(Conn<Io>, u64, B256), NetError>
    where
        Io: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut conn = match remote {
            Some(r) => Conn::connect(io, self.key, r, &self.hello).await?,
            None => Conn::accept(io, self.key, &self.hello).await?,
        };
        let d = self.cfg.disconnect_timeout;
        if conn.remote == self.id {
            bye(&mut conn, DisconnectReason::SelfConnect, d).await;
            return Err(NetError::Rejected(DisconnectReason::SelfConnect));
        }
        let f = framing(conn.eth_version);
        let (head, td) = lock(&self.state).head;
        let ours = Status62 {
            version: u32::try_from(conn.eth_version).unwrap_or(0),
            network_id: self.cfg.network_id,
            td,
            head,
            genesis: self.cfg.genesis,
        };
        let (code, body) = eth::encode(f, &Message::Status(ours))?;
        conn.send(code, &body).await?;
        let (code, body) = conn.recv().await?;
        if code != codes::STATUS {
            bye(&mut conn, DisconnectReason::ProtocolError, d).await;
            return Err(NetError::Protocol("first eth message is not Status"));
        }
        let Message::Status(theirs) = eth::decode(f, code, &body)? else {
            return Err(NetError::Protocol("status"));
        };
        let local = Local {
            network_id: self.cfg.network_id,
            genesis: self.cfg.genesis,
        };
        match theirs.validate(&local, conn.eth_version) {
            Ok(rank) => Ok((conn, rank, theirs.head)),
            Err(e) => {
                bye(&mut conn, DisconnectReason::SubprotocolError, d).await;
                Err(e.into())
            }
        }
    }

    /// Queue `msg` for one peer. Never waits: a full queue is [`NetError::Busy`].
    pub fn send(&self, to: &PeerId, msg: Message) -> Result<(), NetError> {
        let st = lock(&self.state);
        let e = st.peers.get(to).ok_or(NetError::UnknownPeer)?;
        e.tx.try_send(Cmd::Send(msg)).map_err(|_| NetError::Busy)
    }

    fn send_bft(st: &State, to: &[PeerId], msg: &BftMessage) -> usize {
        to.iter()
            .filter_map(|p| st.peers.get(p))
            .filter(|e| e.tx.try_send(Cmd::Send(Message::Bft(msg.clone()))).is_ok())
            .count()
    }

    /// Send our own BFT message to every peer that has not seen it. Returns the peers reached.
    pub fn broadcast_bft(&self, msg: &BftMessage) -> usize {
        let mut st = lock(&self.state);
        let (_, to) = st.gossip.originate(msg);
        Self::send_bft(&st, &to, msg)
    }

    /// After verifying a BFT message received from `from`: send it to the others, once each.
    pub fn relay_bft(&self, msg: &BftMessage, from: &PeerId) -> usize {
        let mut st = lock(&self.state);
        let to = st.gossip.targets(&keccak256(msg.encode_body()), Some(from));
        Self::send_bft(&st, &to, msg)
    }

    /// Count one offence of `id`. `Err` means: end the session.
    fn offend(&self, id: &PeerId, offence: Offence) -> Result<(), DisconnectReason> {
        let now = self.now_ms();
        let mut st = lock(&self.state);
        let trusted = st.trust.is_trusted(id);
        match st.score.record(now, id, offence, trusted) {
            Verdict::Keep => Ok(()),
            Verdict::Disconnect { .. } => Err(DisconnectReason::SubprotocolError),
        }
    }

    /// Close every session and return within about `shutdown_grace`. Sessions get a Disconnect
    /// (reason Quitting); those that do not end in time are aborted. New connections are refused.
    pub async fn shutdown(&self) -> ShutdownReport {
        let entries = {
            let mut st = lock(&self.state);
            st.closed = true;
            std::mem::take(&mut st.peers)
        };
        let mut aborts = Vec::new();
        let mut tasks = Vec::new();
        for e in entries.into_values() {
            // A full queue is dropped with the entry: the session sees its channel closed.
            let _ = e.tx.try_send(Cmd::Close(DisconnectReason::Quitting));
            aborts.push(e.task.abort_handle());
            tasks.push(e.task);
        }
        let total = aborts.len();
        if timeout(self.cfg.shutdown_grace, join_all(tasks))
            .await
            .is_ok()
        {
            return ShutdownReport {
                closed: total,
                aborted: 0,
            };
        }
        let mut aborted = 0;
        for a in aborts {
            if !a.is_finished() {
                a.abort();
                aborted += 1;
            }
        }
        ShutdownReport {
            closed: total - aborted,
            aborted,
        }
    }
}

#[derive(Clone, Copy)]
struct Session {
    id: PeerId,
    seq: u64,
    version: u64,
    inbound: bool,
    rank: u64,
    head: B256,
}

/// Message classes and their per-peer queue (ADR-007 section 2).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Q1: 256, drop oldest.
    Bft,
    /// Q3: 16, reject and score.
    Block,
    /// Q7: 8 requests we must serve, disconnect.
    Request,
    /// No row in the ADR; 64 unsolicited-or-pending responses, reject and score. ponytail:
    /// request tracking (XB-14) makes a response without a request an offence.
    Response,
    /// Q5: 128, reject, no score.
    Tx,
}

struct Queues {
    bft: BoundedQueue<Message>,
    block: BoundedQueue<Message>,
    request: BoundedQueue<Message>,
    response: BoundedQueue<Message>,
    tx: BoundedQueue<Message>,
}

impl Queues {
    fn new() -> Self {
        Self {
            bft: BoundedQueue::new(256, Overflow::DropOldest),
            block: BoundedQueue::new(16, Overflow::Reject),
            request: BoundedQueue::new(8, Overflow::DisconnectPeer),
            response: BoundedQueue::new(64, Overflow::Reject),
            tx: BoundedQueue::new(128, Overflow::Reject),
        }
    }

    fn push(&mut self, c: Class, m: Message) -> Push<Message> {
        match c {
            Class::Bft => &mut self.bft,
            Class::Block => &mut self.block,
            Class::Request => &mut self.request,
            Class::Response => &mut self.response,
            Class::Tx => &mut self.tx,
        }
        .push(m)
    }

    /// Consensus first, then blocks, requests, responses, transactions (ADR-007 section 3).
    fn pop(&mut self) -> Option<Message> {
        self.bft
            .pop()
            .or_else(|| self.block.pop())
            .or_else(|| self.request.pop())
            .or_else(|| self.response.pop())
            .or_else(|| self.tx.pop())
    }

    fn is_empty(&self) -> bool {
        self.bft.is_empty()
            && self.block.is_empty()
            && self.request.is_empty()
            && self.response.is_empty()
            && self.tx.is_empty()
    }
}

fn class(m: &Message) -> Option<Class> {
    Some(match m {
        Message::Bft(_) => Class::Bft,
        Message::NewBlock(_) | Message::NewBlockHashes(_) => Class::Block,
        Message::GetBlockHeaders { .. }
        | Message::GetBlockBodies { .. }
        | Message::GetReceipts { .. } => Class::Request,
        Message::BlockHeaders { .. } | Message::BlockBodies { .. } | Message::Receipts { .. } => {
            Class::Response
        }
        Message::Transactions(_) => Class::Tx,
        Message::Sub { code, .. } if Sub::is_request(*code) => Class::Request,
        Message::Sub { .. } => Class::Response,
        Message::Opaque { code, .. } => match *code {
            codes::POOLED_TXS => Class::Tx,
            codes::GET_NODE_DATA => Class::Request,
            // geth-XDC announces its range on eth/101 (`xdpos2rid`, `eth/protocols/eth/handler.go:306`);
            // nothing here reads it, so it takes no slot of the response queue
            codes::BLOCK_RANGE_UPDATE => return None,
            _ => Class::Response,
        },
        // Status after the handshake is a violation, handled by the caller; 0x08 and 0x09 are drained.
        Message::Status(_) | Message::Drained { .. } => return None,
    })
}

/// Request id of the required-block challenge under eth/101. The application counts up from 1.
const CHALLENGE_ID: u64 = u64::MAX;

/// The required-block check of one session: one header request at a time, the next entry after
/// each good answer.
struct Challenge {
    next: usize,
    deadline: Instant,
    /// The next request is not sent yet.
    unsent: bool,
}

impl Peers {
    fn challenge_request(&self, f: Framing, idx: usize) -> Message {
        let n = self.cfg.required_blocks.get(idx).map_or(0, |(n, _)| *n);
        Message::GetBlockHeaders {
            id: (f == Framing::RequestId).then_some(CHALLENGE_ID),
            query: eth::GetHeaders {
                origin: eth::Origin::Number(n),
                amount: 1,
                skip: 0,
                reverse: false,
            },
        }
    }

    /// The answer to the outstanding challenge. Like geth: an empty answer means the peer has not
    /// reached the height (kept); one header with another hash, or more than one header, is a refusal.
    fn on_challenge(
        &self,
        id: &PeerId,
        c: &mut Option<Challenge>,
        headers: &[Header],
    ) -> Result<(), DisconnectReason> {
        let Some(ch) = c.as_mut() else { return Ok(()) };
        let Some(&(number, want)) = self.cfg.required_blocks.get(ch.next) else {
            *c = None;
            return Ok(());
        };
        let refuse = |got: String| {
            self.refused.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "peer refused  id={}  reason=required-block  number={number}  want={want}  got={got}",
                short(id)
            );
            Err(DisconnectReason::UselessPeer)
        };
        match headers {
            [] => *c = None,
            [h] if h.number == number && h.hash() == want => {
                ch.next += 1;
                ch.deadline = Instant::now() + self.cfg.required_timeout;
                ch.unsent = true;
                if ch.next >= self.cfg.required_blocks.len() {
                    *c = None;
                }
            }
            [h] => return refuse(format!("{} at {}", h.hash(), h.number)),
            _ => return refuse(format!("{} headers", headers.len())),
        }
        Ok(())
    }

    fn enqueue(&self, id: &PeerId, q: &mut Queues, msg: Message) -> Result<(), DisconnectReason> {
        let Some(c) = class(&msg) else { return Ok(()) };
        match q.push(c, msg) {
            Push::Accepted | Push::Evicted(_) | Push::Full(_) => Ok(()),
            Push::Rejected(_) if matches!(c, Class::Block | Class::Response) => {
                self.offend(id, Offence::RateExceeded)
            }
            Push::Rejected(_) => Ok(()),
            Push::Disconnect(_) => {
                self.offend(id, Offence::RateExceeded)?;
                Err(DisconnectReason::SubprotocolError)
            }
        }
    }

    /// One inbound eth message. `Err` ends the session.
    fn on_message(
        &self,
        s: &Session,
        subs: &[(Sub, u64)],
        code: u64,
        body: &[u8],
        q: &mut Queues,
        challenge: &mut Option<Challenge>,
    ) -> Result<(), DisconnectReason> {
        // snap/1 and xpre/1: carried as bytes, decoded by the application
        if let Some((sub, base)) = subs
            .iter()
            .find(|(sub, base)| code >= *base && code < base + sub.codes())
        {
            let msg = Message::Sub {
                sub: *sub,
                code: code - base,
                body: body.to_vec().into(),
            };
            return self.enqueue(&s.id, q, msg);
        }
        if codes::is_bft(code) {
            // Strict decode, size cap and dedup: malformed and oversized BFT messages are scored
            // and dropped (geth drops them silently), the session goes on until the threshold.
            let r = lock(&self.state).gossip.receive(&s.id, code, body);
            return match r {
                Receive::Accepted { msg, .. } => self.enqueue(&s.id, q, Message::Bft(msg)),
                Receive::Reject(off) => self.offend(&s.id, off),
                Receive::Duplicate | Receive::UnknownPeer => Ok(()),
            };
        }
        match eth::decode(framing(s.version), code, body) {
            Ok(Message::Status(_)) => Err(DisconnectReason::ProtocolError),
            // eth/100 has no request id: the first answer after the challenge is its answer
            Ok(Message::BlockHeaders { id, headers })
                if challenge.is_some()
                    && id.is_none_or(|i| i == CHALLENGE_ID)
                    && !challenge.as_ref().is_some_and(|c| c.unsent) =>
            {
                self.on_challenge(&s.id, challenge, &headers)
            }
            Ok(msg) => self.enqueue(&s.id, q, msg),
            Err(_) => {
                // Like geth, an undecodable data-plane message ends the session, trusted or not.
                let _ = self.offend(&s.id, Offence::Malformed);
                Err(DisconnectReason::SubprotocolError)
            }
        }
    }
}

async fn session<Io>(
    mut conn: Conn<Io>,
    p: Peers,
    s: Session,
    subs: Vec<(Sub, u64)>,
    mut rx: mpsc::Receiver<Cmd>,
) where
    Io: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let f = framing(s.version);
    // sent before the application hears of the peer, so that under eth/100 the first header
    // answer is the challenge's
    let mut challenge = None;
    if !p.cfg.required_blocks.is_empty() {
        if let Ok((code, body)) = eth::encode(f, &p.challenge_request(f, 0)) {
            match timeout(p.cfg.write_timeout, conn.send(code, &body)).await {
                Ok(Ok(())) => {
                    challenge = Some(Challenge {
                        next: 0,
                        deadline: Instant::now() + p.cfg.required_timeout,
                        unsent: false,
                    });
                }
                _ => return session_end(&p, &s, conn, DisconnectReason::NetworkError, true).await,
            }
        }
    }
    let _ = p
        .events
        .send(Event::Connected {
            peer: s.id,
            version: s.version,
            rank: s.rank,
            head: s.head,
            inbound: s.inbound,
            subs: subs.iter().map(|(x, _)| *x).collect(),
        })
        .await;
    let mut q = Queues::new();
    let mut last_rx = Instant::now();
    // (reason, true if the connection is already gone and no Disconnect can be sent)
    let (reason, gone) = loop {
        tokio::select! {
            biased;
            cmd = rx.recv() => match cmd {
                Some(Cmd::Send(Message::Sub { sub, code, body })) => {
                    // a capability the peer does not share is never sent
                    if let Some((_, base)) = subs.iter().find(|(x, _)| *x == sub) {
                        match timeout(p.cfg.write_timeout, conn.send(base + code, &body)).await {
                            Ok(Ok(())) => {}
                            _ => break (DisconnectReason::NetworkError, true),
                        }
                    }
                }
                Some(Cmd::Send(m)) => {
                    if let Ok((code, body)) = eth::encode(f, &m) {
                        match timeout(p.cfg.write_timeout, conn.send(code, &body)).await {
                            Ok(Ok(())) => {}
                            _ => break (DisconnectReason::NetworkError, true),
                        }
                    }
                }
                Some(Cmd::Close(r)) => break (r, false),
                None => break (DisconnectReason::Quitting, false),
            },
            permit = p.events.reserve(), if !q.is_empty() => match permit {
                Ok(permit) => {
                    if let Some(msg) = q.pop() {
                        permit.send(Event::Message { peer: s.id, msg });
                    }
                }
                Err(_) => break (DisconnectReason::Quitting, false),
            },
            _ = sleep_until(challenge.as_ref().map_or(last_rx, |c| c.deadline)), if challenge.as_ref().is_some_and(|c| !c.unsent) => {
                p.refused.fetch_add(1, Ordering::Relaxed);
                eprintln!("peer refused  id={}  reason=required-block  no answer in {:?}", short(&s.id), p.cfg.required_timeout);
                break (DisconnectReason::UselessPeer, false)
            }
            _ = sleep_until(last_rx + p.cfg.idle_timeout) => break (DisconnectReason::ReadTimeout, false),
            frame = conn.recv() => {
                last_rx = Instant::now();
                match frame {
                    Ok((code, body)) => {
                        if let Err(r) = p.on_message(&s, &subs, code, &body, &mut q, &mut challenge) {
                            break (r, false);
                        }
                        if let Some(c) = challenge.as_mut().filter(|c| c.unsent) {
                            let req = p.challenge_request(f, c.next);
                            if let Ok((code, body)) = eth::encode(f, &req) {
                                match timeout(p.cfg.write_timeout, conn.send(code, &body)).await {
                                    Ok(Ok(())) => c.unsent = false,
                                    _ => break (DisconnectReason::NetworkError, true),
                                }
                            }
                        }
                    }
                    Err(NetError::Disconnect(_) | NetError::Closed | NetError::Io(_) | NetError::Ecies(_)) => {
                        break (DisconnectReason::NetworkError, true)
                    }
                    Err(NetError::TooLarge { .. }) => {
                        // the cap is checked on the declared size, before the body is read
                        let _ = p.offend(&s.id, Offence::Oversized);
                        break (DisconnectReason::SubprotocolError, false);
                    }
                    Err(_) => {
                        let _ = p.offend(&s.id, Offence::Malformed);
                        break (DisconnectReason::ProtocolError, false);
                    }
                }
            }
        }
    };
    session_end(&p, &s, conn, reason, gone).await
}

async fn session_end<Io>(
    p: &Peers,
    s: &Session,
    mut conn: Conn<Io>,
    reason: DisconnectReason,
    gone: bool,
) where
    Io: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    {
        let mut st = lock(&p.state);
        if st.peers.get(&s.id).is_some_and(|e| e.seq == s.seq) {
            st.peers.remove(&s.id);
            st.gossip.remove_peer(&s.id);
        }
    }
    if !gone {
        bye(&mut conn, reason, p.cfg.disconnect_timeout).await;
    }
    // best effort: the application may have stopped listening
    let sent = timeout(
        p.cfg.disconnect_timeout,
        p.events.send(Event::Disconnected { peer: s.id }),
    )
    .await;
    drop(sent);
}

fn short(id: &PeerId) -> String {
    id.0.iter().take(6).map(|b| format!("{b:02x}")).collect()
}
