//! The vote pool and the timeout pool: one entry per signer and round.
//!
//! geth `utils.Pool.Add` (`consensus/XDPoS/utils/pool.go:44-84`) keeps one entry per pool key and
//! recovered signer, and drops a later message of that signer whatever its bytes (CertiK XDC-07);
//! the key of a vote includes the block, so one signer may still hold one entry for each block of a
//! round. Here a signer holds one entry for each round, whatever it signed: an honest signer votes
//! once per round (ADR-010), so no honest vote is lost, and the memory of a pool is bounded by
//! `rounds * signers` no matter how many variants of a message arrive. The caller adds an entry only
//! after the signer is recovered and is a member of the candidate pool.
//!
//! A message that is not kept costs one scan of the round's entries, no allocation.

use std::collections::BTreeMap;

use alloy_primitives::Address;

#[derive(Debug, Clone)]
pub struct Pool<M> {
    rounds: BTreeMap<u64, Vec<(Address, M)>>,
}

impl<M> Default for Pool<M> {
    fn default() -> Self {
        Self {
            rounds: BTreeMap::new(),
        }
    }
}

impl<M> Pool<M> {
    /// Keeps `msg` if `signer` has no entry for `round`. Returns whether it was kept.
    /// ponytail: a linear scan of the round's entries, at most one per candidate; a map if a round ever holds thousands.
    pub fn add(&mut self, round: u64, signer: Address, msg: M) -> bool {
        let entries = self.rounds.entry(round).or_default();
        if entries.iter().any(|(a, _)| *a == signer) {
            return false;
        }
        entries.push((signer, msg));
        true
    }

    /// The entries of `round`, in the order they arrived.
    pub fn entries(&self, round: u64) -> impl Iterator<Item = (&Address, &M)> {
        self.rounds
            .get(&round)
            .into_iter()
            .flatten()
            .map(|(a, m)| (a, m))
    }

    /// Drops every round below `round` (geth `hygieneVotePool`, `vote.go:464-483`).
    pub fn prune_below(&mut self, round: u64) {
        self.rounds = self.rounds.split_off(&round);
    }

    pub fn clear(&mut self) {
        self.rounds.clear();
    }

    /// Number of messages held.
    pub fn len(&self) -> usize {
        self.rounds.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.rounds.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_entry_per_signer_and_round() {
        let mut p = Pool::default();
        let a = Address::repeat_byte(1);
        assert!(p.add(5, a, 1));
        assert!(!p.add(5, a, 2), "a variant of the same signer is dropped");
        assert!(p.add(6, a, 3));
        assert_eq!(p.len(), 2);
        assert_eq!(p.entries(5).map(|(_, m)| *m).collect::<Vec<_>>(), [1]);
        p.prune_below(6);
        assert_eq!(p.len(), 1);
        p.clear();
        assert!(p.is_empty());
    }
}
