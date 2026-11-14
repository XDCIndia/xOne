//! snap/1 and xpre/1 served from the node's retained state window (XB-17, `spec/snap/`).
//!
//! A request names a state root. The node answers it when the root is the state root of a canonical
//! block inside its retained window (`core::RETENTION` blocks below the head): the plain rows of that
//! block (`Store::state_rows`) become an `xone_snap::server::Snapshot`, which answers with geth's
//! limits and the ADR-007 Q15 bounds (`Server::handle`). Any other root is answered empty, as geth
//! answers a root it does not hold. xpre/1 answers the address or slot behind a hashed key from the
//! same rows.
//!
//! One task serves every peer in arrival order, so at most one request is in work.
//!
//! ponytail: a snapshot is the whole state in memory, built under the store's read lock (imports
//! wait while it builds) and cached for the last two roots; a table-backed snapshot over the
//! `HashedAccounts`/`HashedStorages` tables replaces it when the trie is persisted.

use std::{sync::Arc, time::Instant};

use alloy_primitives::{Bytes, B256};
use tokio::sync::mpsc::Receiver;
use xone_net::{eth::Message as Eth, peers::Peers, rlpx::PeerId, rlpx::Sub};
use xone_snap::{
    codec::Message,
    preimage::{self, Preimages},
    server::{byte_codes_from, empty_reply, Handled, Limits, Server, Snapshot},
};
use xone_sync::store::Store;

use super::core::{Core, RETENTION};

/// Roots whose snapshot is kept: the pivot of a client and one gap block it asks for meanwhile.
const CACHED: usize = 2;

pub struct SnapServer {
    core: Arc<Core>,
    peers: Peers,
    cache: Vec<(B256, Server)>,
    start: Instant,
}

/// One request of a peer: the capability, the code relative to it and the body.
pub type Request = (PeerId, Sub, u64, Bytes);

fn root_of(m: &Message) -> Option<B256> {
    match m {
        Message::GetAccountRange(r) => Some(r.root),
        Message::GetStorageRanges(r) => Some(r.root),
        Message::GetTrieNodes(r) => Some(r.root),
        _ => None,
    }
}

impl SnapServer {
    pub fn new(core: Arc<Core>, peers: Peers) -> Self {
        Self {
            core,
            peers,
            cache: Vec::new(),
            start: Instant::now(),
        }
    }

    /// The canonical block in the retained window whose state root is `root`.
    fn block_of(&self, root: B256) -> Option<(u64, B256)> {
        let h = self.core.hist.read();
        let tip = h.tip_number();
        (tip.saturating_sub(RETENTION)..=tip).rev().find_map(|n| {
            let x = h.header_at(n)?;
            (x.state_root == root).then(|| (n, x.hash()))
        })
    }

    /// The server of `root`, built if it is in the window and not cached.
    async fn server(&mut self, root: B256) -> Option<&mut Server> {
        if let Some(i) = self.cache.iter().position(|(r, _)| *r == root) {
            let hit = self.cache.remove(i);
            self.cache.push(hit);
            return self.cache.last_mut().map(|(_, s)| s);
        }
        let (n, hash) = self.block_of(root)?;
        let core = self.core.clone();
        let t = Instant::now();
        let built = tokio::task::spawn_blocking(move || {
            let rows = core.chain.with_store(|s| s.state_rows(hash)).ok()?;
            let codes = core.hist.read().codes.clone();
            let snap = Snapshot::from_rows(&rows.accounts, &rows.storage, codes);
            Some((snap, rows.accounts.len(), rows.storage.len()))
        })
        .await
        .ok()??;
        let (snap, accounts, slots) = built;
        if snap.root() != root {
            eprintln!(
                "snap: the rows of block {n} rebuild {} instead of {root}; not served",
                snap.root()
            );
            return None;
        }
        eprintln!(
            "snap: serving root {root} of block {n}: {accounts} accounts, {slots} slots, built in {} ms",
            t.elapsed().as_millis()
        );
        if self.cache.len() >= CACHED {
            self.cache.remove(0);
        }
        self.cache
            .push((root, Server::new(snap, Limits::default())));
        self.cache.last_mut().map(|(_, s)| s)
    }

    async fn answer(&mut self, peer: PeerId, sub: Sub, code: u64, body: &[u8]) -> Option<Eth> {
        let now = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (code, body) = match sub {
            Sub::Snap => {
                // an undecodable request is dropped; the peer times out
                let msg = Message::decode(code, body).ok()?;
                let empty = empty_reply(&msg)?;
                // Bytecode is content-addressed: answered from the code store, no root needed.
                if let Message::GetByteCodes(r) = &msg {
                    let reply = {
                        let hist = self.core.hist.read();
                        Message::ByteCodes(byte_codes_from(r, |h| hist.codes.get(h).cloned()))
                    };
                    return Some(Eth::Sub {
                        sub,
                        code: reply.code(),
                        body: reply.encode().into(),
                    });
                }
                let root = root_of(&msg).or_else(|| self.cache.last().map(|(r, _)| *r));
                let reply = match root {
                    Some(r) => match self.server(r).await {
                        Some(s) => match s.handle(peer, now, &msg) {
                            Handled::Reply(m) | Handled::RateExceeded { reply: m, .. } => m,
                            Handled::Disconnect => return None,
                        },
                        None => empty,
                    },
                    None => empty,
                };
                (reply.code(), reply.encode())
            }
            Sub::Xpre => {
                let preimage::Message::Get(req) = preimage::Message::decode(code, body).ok()?
                else {
                    return None;
                };
                // from the node's preimage table: whatever execution and installs have seen
                let reply = Preimages {
                    id: req.id,
                    preimages: self.core.chain.with_store(|st| {
                        req.hashes
                            .iter()
                            .take(preimage::MAX_PREIMAGES)
                            .filter_map(|h| st.preimage(*h))
                            .collect()
                    }),
                };
                let m = preimage::Message::Answer(reply);
                (m.code(), m.encode())
            }
        };
        Some(Eth::Sub {
            sub,
            code,
            body: body.into(),
        })
    }

    pub async fn run(mut self, mut rx: Receiver<Request>) {
        while let Some((peer, sub, code, body)) = rx.recv().await {
            if let Some(reply) = self.answer(peer, sub, code, &body).await {
                // a full queue of the peer drops the answer; it asks again
                let _ = self.peers.send(&peer, reply);
            }
        }
    }
}
