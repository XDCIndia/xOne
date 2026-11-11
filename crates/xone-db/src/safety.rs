//! Safety store interface (ADR-010). A separate store from chaindata: it holds
//! the signer's non-reversible record and is never touched by unwind, set-head,
//! restore or snapshot import. The store treats intent and record as opaque
//! bytes; `xone-consensus` owns their encoding.

/// Proof that the pair was durable (fsync of file and directory returned) before
/// this value was made. `seq` is the store's own write counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ack {
    pub seq: u64,
}

/// The last persisted pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub intent: Vec<u8>,
    pub record: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StoreError {
    /// A write or sync failed. The caller must stop signing until restart (ADR-005 section 9).
    #[error("safety store I/O failure")]
    Io,
    /// A store file exists but holds no valid slot. Never treated as an empty store.
    #[error("safety store corrupt")]
    Corrupt,
}

pub trait SafetyStore {
    /// `None` only for a store that was never written. Any record returned has
    /// been synced by the load itself, so a record that was only in the page
    /// cache after a crash is made durable before the signer relies on it.
    fn load(&mut self) -> Result<Option<Loaded>, StoreError>;
    /// Write intent and record as one unit, then sync. Returns only after the sync.
    fn persist_intent(&mut self, intent: &[u8], record: &[u8]) -> Result<Ack, StoreError>;
}

#[cfg(any(test, feature = "fault-injection"))]
pub use mem::{Fault, MemStore};

#[cfg(any(test, feature = "fault-injection"))]
mod mem {
    use super::*;

    /// Where the next `persist_intent` fails. Each is one-shot.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Fault {
        /// Nothing written.
        BeforeWrite,
        /// Written, not synced. A power cycle may keep or drop it.
        AfterWrite,
        /// Sync returned an error. The write may or may not survive.
        FailSync,
        /// Synced, but the caller sees no ack.
        AfterSync,
    }

    /// In-memory store: `durable` survives a power cycle, `pending` may.
    #[derive(Default)]
    pub struct MemStore {
        durable: Option<(u64, Loaded)>,
        pending: Option<(u64, Loaded)>,
        history: Vec<Option<(u64, Loaded)>>,
        fault: Option<Fault>,
    }

    impl MemStore {
        pub fn arm(&mut self, f: Fault) {
            self.fault = Some(f);
        }

        pub fn disarm(&mut self) {
            self.fault = None;
        }

        /// Crash or power loss. `keep_unsynced` decides if an unsynced write reached the device.
        pub fn power_cycle(&mut self, keep_unsynced: bool) {
            if let Some(p) = self.pending.take() {
                if keep_unsynced {
                    self.commit(p);
                }
            }
            self.fault = None;
        }

        /// Number of durable states so far (candidates for a stale backup).
        pub fn history_len(&self) -> usize {
            self.history.len()
        }

        /// Replace the store with an older durable state (stale backup). `None` is a wiped store.
        pub fn restore(&mut self, idx: Option<usize>) {
            self.durable = idx.and_then(|i| self.history[i].clone());
            self.pending = None;
        }

        fn commit(&mut self, p: (u64, Loaded)) {
            self.history.push(Some(p.clone()));
            self.durable = Some(p);
        }
    }

    impl SafetyStore for MemStore {
        fn load(&mut self) -> Result<Option<Loaded>, StoreError> {
            Ok(self.durable.as_ref().map(|d| d.1.clone()))
        }

        fn persist_intent(&mut self, intent: &[u8], record: &[u8]) -> Result<Ack, StoreError> {
            let seq = self.durable.as_ref().map_or(0, |d| d.0) + 1;
            let frame = (
                seq,
                Loaded {
                    intent: intent.to_vec(),
                    record: record.to_vec(),
                },
            );
            match self.fault.take() {
                Some(Fault::BeforeWrite) => Err(StoreError::Io),
                Some(Fault::AfterWrite | Fault::FailSync) => {
                    self.pending = Some(frame);
                    Err(StoreError::Io)
                }
                Some(Fault::AfterSync) => {
                    self.commit(frame);
                    Err(StoreError::Io)
                }
                None => {
                    self.commit(frame);
                    Ok(Ack { seq })
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn faults_and_power_cycle() {
            let mut s = MemStore::default();
            assert_eq!(s.load(), Ok(None));
            assert_eq!(s.persist_intent(b"i1", b"r1"), Ok(Ack { seq: 1 }));
            s.arm(Fault::BeforeWrite);
            assert!(s.persist_intent(b"i2", b"r2").is_err());
            assert_eq!(s.load().unwrap().unwrap().record, b"r1");
            s.arm(Fault::AfterWrite);
            assert!(s.persist_intent(b"i2", b"r2").is_err());
            s.power_cycle(false);
            assert_eq!(s.load().unwrap().unwrap().record, b"r1");
            s.arm(Fault::AfterWrite);
            assert!(s.persist_intent(b"i2", b"r2").is_err());
            s.power_cycle(true);
            assert_eq!(s.load().unwrap().unwrap().record, b"r2");
            s.arm(Fault::AfterSync);
            assert!(s.persist_intent(b"i3", b"r3").is_err());
            assert_eq!(s.load().unwrap().unwrap().record, b"r3");
            s.restore(Some(0));
            assert_eq!(s.load().unwrap().unwrap().record, b"r1");
            s.restore(None);
            assert_eq!(s.load(), Ok(None));
        }
    }
}
