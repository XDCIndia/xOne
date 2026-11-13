//! Transaction senders recovered ahead of execution on worker threads that live as long as the node
//! (geth `SenderCacher`, `core/sender_cacher.go:104`; `insertChain` recovers the senders of the whole
//! batch before it executes it, `core/blockchain.go:2526`). The chain service gives every body that
//! arrives to [`Senders::warm`] while the blocks before it execute; the import takes the results by
//! transaction hash ([`Senders::of`]) and recovers the rest on the same threads.
//!
//! A result is what `tx::decode` recovers from the same bytes, so a held one is never wrong, only
//! early; a transaction that does not decode is never held, and the executor decodes it again and
//! reports its error.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex, PoisonError,
    },
};

use alloy_primitives::{keccak256, Address, Bytes, B256};
use xone_executor::tx;

/// Transactions per job.
const CHUNK: usize = 256;
/// ponytail: warming stops while this many transactions wait for a thread (about 25 blocks of
/// 20,000), so a peer that floods bodies costs no more than that; the import recovers the rest.
const MAX_QUEUED: usize = 500_000;
/// ponytail: the held results are dropped when this many are held (results of blocks that were not
/// imported: a refused answer, a block another peer gave first). An LRU if that ever matters.
const MAX_HELD: usize = 1 << 20;

type Job = Box<dyn FnOnce() + Send>;

struct Shared {
    chain_id: u64,
    held: Mutex<HashMap<B256, Address>>,
    queued: AtomicUsize,
}

impl Shared {
    fn held(&self) -> std::sync::MutexGuard<'_, HashMap<B256, Address>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn recover(&self, raw: &[u8]) -> Option<Address> {
        tx::decode(raw, self.chain_id).ok().map(|t| t.from)
    }
}

pub struct Senders {
    shared: Arc<Shared>,
    jobs: mpsc::Sender<Job>,
    workers: usize,
}

impl Senders {
    /// One worker per core, as geth's `NewTxSenderCacher(runtime.NumCPU())`.
    pub fn new(chain_id: u64) -> Self {
        let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        Self::with_threads(chain_id, threads)
    }

    pub fn with_threads(chain_id: u64, threads: usize) -> Self {
        let (jobs, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let workers = (0..threads)
            .filter(|i| {
                let rx = rx.clone();
                std::thread::Builder::new()
                    .name(format!("senders-{i}"))
                    .spawn(move || loop {
                        let job = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                        let Ok(job) = job else { return };
                        // a job that panics loses its results, not the worker: the import recovers them
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                    })
                    .is_ok()
            })
            .count();
        Self {
            shared: Arc::new(Shared {
                chain_id,
                held: Mutex::new(HashMap::new()),
                queued: AtomicUsize::new(0),
            }),
            jobs,
            workers,
        }
    }

    fn run(&self, job: Job) {
        if self.workers == 0 {
            return job();
        }
        if let Err(mpsc::SendError(job)) = self.jobs.send(job) {
            job();
        }
    }

    /// Starts the recovery of `raws` (consensus encodings) and returns at once.
    pub fn warm(&self, raws: Vec<Bytes>) {
        let s = &self.shared;
        if raws.is_empty() || s.queued.load(Ordering::Relaxed) + raws.len() > MAX_QUEUED {
            return;
        }
        s.queued.fetch_add(raws.len(), Ordering::Relaxed);
        for part in raws.chunks(CHUNK) {
            let (s, part) = (s.clone(), part.to_vec());
            self.run(Box::new(move || {
                let done: Vec<_> = part
                    .iter()
                    .filter_map(|raw| Some((keccak256(raw), s.recover(raw)?)))
                    .collect();
                let mut held = s.held();
                if held.len() + done.len() > MAX_HELD {
                    held.clear();
                }
                held.extend(done);
                drop(held);
                s.queued.fetch_sub(part.len(), Ordering::Relaxed);
            }));
        }
    }

    /// The senders of `raws` in order: the warmed ones, the rest recovered now on the workers (after
    /// the warm jobs queued before them, whose results they take). `None` when one does not decode.
    pub fn of(&self, raws: &[Bytes]) -> Option<Vec<Address>> {
        let hashes: Vec<B256> = raws.iter().map(keccak256).collect();
        let mut out: Vec<Option<Address>> = {
            let mut held = self.shared.held();
            hashes.iter().map(|h| held.remove(h)).collect()
        };
        let missing: Vec<usize> = (0..raws.len()).filter(|i| out[*i].is_none()).collect();
        let (tx, rx) = mpsc::channel();
        for part in missing.chunks(CHUNK) {
            let items: Vec<(usize, B256, Bytes)> = part
                .iter()
                .map(|i| (*i, hashes[*i], raws[*i].clone()))
                .collect();
            let (s, tx) = (self.shared.clone(), tx.clone());
            self.run(Box::new(move || {
                for (i, h, raw) in items {
                    let a = s.held().remove(&h).or_else(|| s.recover(&raw));
                    let _ = tx.send((i, a));
                }
            }));
        }
        drop(tx);
        for (i, a) in rx {
            out[i] = a;
        }
        out.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use alloy_rlp::Encodable;
    use secp256k1::{Message, SecretKey, SECP256K1};

    /// A signed legacy EIP-155 transfer of `nonce` on chain 2040, and its signer.
    fn signed(nonce: u64) -> (Bytes, Address) {
        let key = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pk = key.public_key(SECP256K1).serialize_uncompressed();
        let signer = Address::from_slice(&keccak256(&pk[1..])[12..]);
        let fields = |v: u64, r: U256, s: U256| {
            let mut p = vec![];
            nonce.encode(&mut p);
            1u64.encode(&mut p);
            21_000u64.encode(&mut p);
            Address::repeat_byte(1).encode(&mut p);
            U256::from(1).encode(&mut p);
            Bytes::new().encode(&mut p);
            v.encode(&mut p);
            r.encode(&mut p);
            s.encode(&mut p);
            let mut out = vec![];
            alloy_rlp::Header {
                list: true,
                payload_length: p.len(),
            }
            .encode(&mut out);
            out.extend(p);
            out
        };
        let digest = keccak256(fields(2040, U256::ZERO, U256::ZERO));
        let sig = SECP256K1.sign_ecdsa_recoverable(&Message::from_digest(digest.0), &key);
        let (id, rs) = sig.serialize_compact();
        let v = 35 + 2 * 2040 + u64::try_from(i32::from(id)).unwrap();
        let (r, s) = (
            U256::from_be_slice(&rs[..32]),
            U256::from_be_slice(&rs[32..]),
        );
        (Bytes::from(fields(v, r, s)), signer)
    }

    /// Warmed or not, with workers or none, the answer is the sequential `tx::decode`'s; a body with
    /// one transaction that does not decode is `None`; nothing stays held once the import took it.
    #[test]
    fn senders_are_those_of_the_sequential_decode() {
        let txs: Vec<(Bytes, Address)> = (0..600).map(signed).collect();
        let raws: Vec<Bytes> = txs.iter().map(|t| t.0.clone()).collect();
        let want: Vec<Address> = txs.iter().map(|t| t.1).collect();
        let seq: Vec<Address> = raws
            .iter()
            .map(|r| tx::decode(r, 2040).unwrap().from)
            .collect();
        assert_eq!(seq, want);
        for threads in [0, 1, 4] {
            let s = Senders::with_threads(2040, threads);
            assert_eq!(s.of(&raws).unwrap(), want);
            s.warm(raws[..300].to_vec());
            assert_eq!(s.of(&raws).unwrap(), want);
            // one worker runs the jobs in order, so the import takes every warmed result (with more,
            // a warm job still running when the import recovers the same transaction leaves its
            // result held until `MAX_HELD`)
            let ordered = threads <= 1;
            assert!(!ordered || s.shared.held().is_empty());
            let mut bad = raws.clone();
            bad[450] = Bytes::from_static(&[0xc1, 0x80]);
            s.warm(bad.clone());
            assert_eq!(s.of(&bad), None);
            assert!(!ordered || s.shared.held().is_empty());
            // signed for chain 2040: another chain recovers none
            assert_eq!(Senders::with_threads(1, threads).of(&raws[..1]), None);
        }
    }
}
