//! `NetSource`: the blocking [`PeerSource`] of `xone-sync` over `xone-net` sessions. A test tool:
//! it owns a current-thread runtime and waits for answers with a bound, so the follow driver stays
//! pure. Replies are matched to the request by kind and, on eth/101, by request id; an unsolicited
//! message that is not a push is dropped.
//!
//! A peer's head hash is the head of its Status, then what it last announced (`NewBlock`,
//! `NewBlockHashes`): eth/100 and eth/101 carry no other head.
#![forbid(unsafe_code)]

use alloy_primitives::{Bytes, B256};
use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};
use tokio::{runtime::Runtime, sync::mpsc::Receiver, time::timeout};
use xone_consensus::verify::Body;
use xone_net::{
    eth::{BlockBody, GetHeaders, Message, Origin, Raw},
    peers::{Config, Event, Peers},
    rlpx::PeerId,
};
use xone_primitives::Header;
use xone_sync::{
    follow::{PeerError, PeerInfo, PeerSource, Push},
    store::Block,
};

pub struct NetSource {
    rt: Runtime,
    pub peers: Peers,
    rx: Receiver<Event>,
    /// version and rank per connected peer
    sessions: HashMap<PeerId, (u64, u64)>,
    heads: HashMap<PeerId, B256>,
    pushes: VecDeque<Push<PeerId>>,
    inbox: VecDeque<(PeerId, Message)>,
    next_id: u64,
    pub wait: Duration,
    /// Highest block number any peer announced or pushed, and its hash.
    pub announced: (u64, B256),
}

/// `Raw` of a transaction in a body is the item as sent: a legacy transaction is the RLP list, a
/// typed one an RLP string whose content is the consensus encoding (the shape `Body` keeps).
fn tx_bytes(r: &Raw) -> Bytes {
    let mut b: &[u8] = &r.0;
    match alloy_rlp::Header::decode(&mut b) {
        Ok(h) if !h.list => Bytes::copy_from_slice(&b[..h.payload_length.min(b.len())]),
        _ => r.0.clone(),
    }
}

fn body(b: &BlockBody) -> Body {
    Body {
        transactions: b.transactions.iter().map(tx_bytes).collect(),
        uncles: b.uncles.clone(),
        withdrawals: None,
    }
}

fn err(e: impl std::fmt::Display) -> PeerError {
    PeerError(e.to_string())
}

impl NetSource {
    pub fn new(cfg: Config, key: secp256k1::SecretKey) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (peers, rx) = {
            let _g = rt.enter();
            Peers::new(cfg, key)
        };
        Ok(Self {
            rt,
            peers,
            rx,
            sessions: HashMap::new(),
            heads: HashMap::new(),
            pushes: VecDeque::new(),
            inbox: VecDeque::new(),
            next_id: 1,
            wait: Duration::from_secs(5),
            announced: (0, B256::ZERO),
        })
    }

    pub fn dial(&mut self, addr: std::net::SocketAddr, id: PeerId) -> Result<PeerId, String> {
        let p = self.peers.clone();
        let id = self
            .rt
            .block_on(async move { p.dial(addr, id).await })
            .map_err(|e| e.to_string())?;
        // the session is usable once its Connected event has been read
        for _ in 0..50 {
            if self.sessions.contains_key(&id) {
                return Ok(id);
            }
            self.pump(Duration::from_millis(100), |_| false);
        }
        Err("no Connected event".into())
    }

    pub fn shutdown(&mut self) {
        let p = self.peers.clone();
        self.rt.block_on(async move {
            p.shutdown().await;
        });
    }

    /// Takes events for at most `wait`, until `done` says the inbox has what the caller needs.
    fn pump(&mut self, wait: Duration, done: impl Fn(&VecDeque<(PeerId, Message)>) -> bool) {
        let deadline = std::time::Instant::now() + wait;
        loop {
            if done(&self.inbox) {
                return;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let ev = self
                .rt
                .block_on(async { timeout(left, self.rx.recv()).await });
            let Ok(Some(ev)) = ev else { return };
            self.take(ev);
            while let Ok(ev) = self.rx.try_recv() {
                self.take(ev);
            }
        }
    }

    fn take(&mut self, ev: Event) {
        match ev {
            Event::Connected {
                peer,
                version,
                rank,
                head,
                ..
            } => {
                self.sessions.insert(peer, (version, rank));
                self.heads.insert(peer, head);
            }
            Event::Disconnected { peer } => {
                self.sessions.remove(&peer);
                self.heads.remove(&peer);
            }
            Event::Message { peer, msg } => match msg {
                Message::NewBlock(nb) => {
                    let block = Block::new(
                        nb.block.header.clone(),
                        body(&BlockBody {
                            transactions: nb.block.transactions.clone(),
                            uncles: nb.block.uncles.clone(),
                        }),
                    );
                    self.heads.insert(peer, block.hash());
                    if block.number() >= self.announced.0 {
                        self.announced = (block.number(), block.hash());
                    }
                    if let Some(s) = self.sessions.get_mut(&peer) {
                        s.1 = u64::try_from(nb.td).unwrap_or(u64::MAX);
                    }
                    self.pushes.push_back(Push::NewBlock {
                        peer,
                        rank: u64::try_from(nb.td).unwrap_or(u64::MAX),
                        block: Box::new(block),
                    });
                }
                Message::NewBlockHashes(v) => {
                    for a in v {
                        self.heads.insert(peer, a.hash);
                        if a.number >= self.announced.0 {
                            self.announced = (a.number, a.hash);
                        }
                        self.pushes.push_back(Push::Announce {
                            peer,
                            hash: a.hash,
                            number: a.number,
                        });
                    }
                }
                m @ (Message::BlockHeaders { .. } | Message::BlockBodies { .. }) => {
                    self.inbox.push_back((peer, m));
                }
                // We serve nothing, and a peer that does not answer is dropped: reply empty, as the
                // genesis holder of `run.sh` does.
                Message::GetBlockHeaders { id, .. } => {
                    let _ = self.peers.send(
                        &peer,
                        Message::BlockHeaders {
                            id,
                            headers: vec![],
                        },
                    );
                }
                Message::GetBlockBodies { id, .. } => {
                    let _ = self
                        .peers
                        .send(&peer, Message::BlockBodies { id, bodies: vec![] });
                }
                Message::GetReceipts { id, .. } => {
                    let _ = self.peers.send(
                        &peer,
                        Message::Receipts {
                            id,
                            last_incomplete: false,
                            per_block: vec![],
                        },
                    );
                }
                _ => {} // transactions, BFT gossip: not for sync
            },
        }
    }

    /// Sends `msg` (with a fresh request id on eth/101) and waits for the reply that `is_reply`
    /// accepts for that id.
    fn ask(
        &mut self,
        peer: &PeerId,
        mk: impl Fn(Option<u64>) -> Message,
        is_reply: impl Fn(&Message, Option<u64>) -> bool,
    ) -> Result<Message, PeerError> {
        let version = self
            .sessions
            .get(peer)
            .map(|s| s.0)
            .ok_or_else(|| err("no session"))?;
        let id = (version == 101).then(|| {
            self.next_id += 1;
            self.next_id
        });
        self.peers.send(peer, mk(id)).map_err(err)?;
        let p = *peer;
        self.pump(self.wait, |q| {
            q.iter().any(|(f, m)| *f == p && is_reply(m, id))
        });
        let at = self
            .inbox
            .iter()
            .position(|(f, m)| *f == p && is_reply(m, id))
            .ok_or_else(|| err("timeout"))?;
        self.inbox
            .remove(at)
            .map(|(_, m)| m)
            .ok_or_else(|| err("gone"))
    }
}

impl PeerSource for NetSource {
    type Peer = PeerId;

    fn peers(&mut self) -> Vec<PeerInfo<PeerId>> {
        self.pump(Duration::ZERO, |_| true);
        self.sessions
            .iter()
            .map(|(p, (_, rank))| PeerInfo {
                peer: *p,
                head: self.heads.get(p).copied().unwrap_or(B256::ZERO),
                rank: *rank,
            })
            .collect()
    }

    fn poll(&mut self) -> Vec<Push<PeerId>> {
        self.pump(Duration::from_millis(50), |_| false);
        self.pushes.drain(..).collect()
    }

    fn headers_by_number(
        &mut self,
        peer: &PeerId,
        from: u64,
        max: usize,
    ) -> Result<Vec<Header>, PeerError> {
        let amount = u64::try_from(max).unwrap_or(u64::MAX);
        let m = self.ask(
            peer,
            |id| Message::GetBlockHeaders {
                id,
                query: GetHeaders {
                    origin: Origin::Number(from),
                    amount,
                    skip: 0,
                    reverse: false,
                },
            },
            |m, id| matches!(m, Message::BlockHeaders { id: i, .. } if *i == id),
        )?;
        match m {
            Message::BlockHeaders { headers, .. } => Ok(headers),
            _ => Err(err("unexpected reply")),
        }
    }

    fn header_by_hash(&mut self, peer: &PeerId, hash: B256) -> Result<Option<Header>, PeerError> {
        let m = self.ask(
            peer,
            |id| Message::GetBlockHeaders {
                id,
                query: GetHeaders {
                    origin: Origin::Hash(hash),
                    amount: 1,
                    skip: 0,
                    reverse: false,
                },
            },
            |m, id| matches!(m, Message::BlockHeaders { id: i, .. } if *i == id),
        )?;
        match m {
            Message::BlockHeaders { mut headers, .. } => Ok(headers.pop()),
            _ => Err(err("unexpected reply")),
        }
    }

    fn bodies(&mut self, peer: &PeerId, headers: &[Header]) -> Result<Vec<Body>, PeerError> {
        let hashes: Vec<B256> = headers.iter().map(Header::hash).collect();
        let m = self.ask(
            peer,
            |id| Message::GetBlockBodies {
                id,
                hashes: hashes.clone(),
            },
            |m, id| matches!(m, Message::BlockBodies { id: i, .. } if *i == id),
        )?;
        match m {
            Message::BlockBodies { bodies, .. } => Ok(bodies.iter().map(body).collect()),
            _ => Err(err("unexpected reply")),
        }
    }
}
