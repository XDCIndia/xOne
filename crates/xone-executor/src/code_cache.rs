//! Analysed bytecode by code hash, kept across transactions and blocks (ethrex keeps the jumpdest
//! analysis the same way). Code is immutable by hash, so an entry never goes stale; the cache only
//! saves `Bytecode::new_raw` (the jumpdest scan and the padding copy) on every account load.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, PoisonError},
};

use alloy_primitives::{Bytes, B256};
use revm::state::Bytecode;

/// The default bound: 64 MiB of contract code.
pub const DEFAULT_CODE_CACHE_BYTES: usize = 64 << 20;

/// A bounded map from code hash to analysed [`Bytecode`]; the oldest entries leave first once the
/// raw code held passes the bound.
pub struct CodeCache {
    limit: usize,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    map: HashMap<B256, Bytecode>,
    order: VecDeque<(B256, usize)>,
    bytes: usize,
}

impl Default for CodeCache {
    fn default() -> Self {
        Self::new(DEFAULT_CODE_CACHE_BYTES)
    }
}

impl CodeCache {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            inner: Mutex::new(Inner::default()),
        }
    }

    pub fn get(&self, hash: &B256) -> Option<Bytecode> {
        self.lock().map.get(hash).cloned()
    }

    /// Analyses `code` (stored under `hash`) and keeps the result, unless it alone passes the bound.
    pub fn analyse(&self, hash: B256, code: Bytes) -> Bytecode {
        let len = code.len();
        let b = Bytecode::new_raw(code);
        if len > self.limit {
            return b;
        }
        let mut g = self.lock();
        if g.map.insert(hash, b.clone()).is_none() {
            g.order.push_back((hash, len));
            g.bytes += len;
        }
        while g.bytes > self.limit {
            let Some((h, n)) = g.order.pop_front() else {
                break;
            };
            g.map.remove(&h);
            g.bytes -= n;
        }
        b
    }

    /// The number of entries and the raw code bytes they hold.
    pub fn size(&self) -> (usize, usize) {
        let g = self.lock();
        (g.map.len(), g.bytes)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::keccak256;

    #[test]
    fn a_cached_bytecode_is_the_fresh_analysis() {
        let c = CodeCache::new(1_000);
        // PUSH1 4, JUMP, INVALID, JUMPDEST, STOP
        let code = Bytes::from_static(&[0x60, 0x04, 0x56, 0xfe, 0x5b, 0x00]);
        let h = keccak256(&code);
        let first = c.analyse(h, code.clone());
        assert_eq!(first, Bytecode::new_raw(code.clone()));
        assert_eq!(c.get(&h), Some(Bytecode::new_raw(code)));
        assert_eq!(c.size(), (1, 6));
    }

    #[test]
    fn the_oldest_code_leaves_once_the_bound_is_passed() {
        let c = CodeCache::new(10);
        let code = |b: u8| Bytes::from(vec![b; 4]);
        for b in 0..3u8 {
            c.analyse(keccak256(code(b)), code(b));
        }
        // 12 bytes held over a bound of 10: the first went
        assert_eq!(c.size(), (2, 8));
        assert_eq!(c.get(&keccak256(code(0))), None);
        assert!(c.get(&keccak256(code(2))).is_some());
        // larger than the bound alone: analysed, not kept
        let big = Bytes::from(vec![0u8; 11]);
        c.analyse(keccak256(&big), big.clone());
        assert_eq!(c.get(&keccak256(&big)), None);
    }
}
