//! The comeback scan shared by V1, V2 before and V2 after `TIPUpgradePenalty` (geth
//! `eth/hooks/hooks.go:243-296`, `engine_v2_hooks.go:388-455`, `:466-552`).
//!
//! A sign transaction counts for a comeback when it signs a block whose number is a multiple of
//! `MergeSignRange` (15) that the scan has already reached, the block of the transaction itself
//! included. The scan therefore runs in ascending block order: run backwards, a signature would be
//! looked up before the block it signs was flagged and would not count.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use alloy_primitives::{Address, B256};

use crate::chain::{body_at, BlockReader, Error, SignTx, MERGE_SIGN_RANGE};

/// What the scan tells its caller, which decides what a sign transaction does.
pub trait Visitor {
    /// Nothing is left to decide; checked before the body of each block is read.
    fn done(&self) -> bool;
    fn sign(&mut self, tx: &SignTx);
}

/// `blocks` ascending (number, hash). An absent body is an error, not zero signatures.
pub fn scan<R: BlockReader>(
    reader: &R,
    blocks: &[(u64, B256)],
    v: &mut impl Visitor,
) -> Result<(), Error> {
    let mut flagged = BTreeSet::new();
    for &(number, hash) in blocks {
        if v.done() {
            break;
        }
        if number % MERGE_SIGN_RANGE == 0 {
            flagged.insert(hash);
        }
        for tx in body_at(reader, hash, number)? {
            if flagged.contains(&tx.signed) {
                v.sign(&tx);
            }
        }
    }
    Ok(())
}

/// Candidates that come back: removes from `pending` the first entry equal to each signer.
pub struct Release {
    pub pending: Vec<Address>,
}

impl Visitor for Release {
    fn done(&self) -> bool {
        self.pending.is_empty()
    }
    fn sign(&mut self, tx: &SignTx) {
        if let Some(i) = self.pending.iter().position(|a| *a == tx.sender) {
            self.pending.remove(i);
        }
    }
}

/// Qualifying sign transactions per signer, each (signer, signed hash) pair once when `dedup`
/// (`ParoleDedup`, CertiK XDC-12; `engine_v2_hooks.go:500-531`).
pub struct Tally {
    pub dedup: bool,
    pub counts: BTreeMap<Address, u64>,
    seen: BTreeSet<(Address, B256)>,
}

impl Tally {
    pub fn new(dedup: bool) -> Self {
        Self {
            dedup,
            counts: BTreeMap::new(),
            seen: BTreeSet::new(),
        }
    }
}

impl Visitor for Tally {
    fn done(&self) -> bool {
        false
    }
    fn sign(&mut self, tx: &SignTx) {
        if self.dedup && !self.seen.insert((tx.sender, tx.signed)) {
            return;
        }
        *self.counts.entry(tx.sender).or_default() += 1;
    }
}
