//! XDC special transactions: a transaction to 0x89 (block signers) or 0x90 (randomize).
//!
//! * only a candidate may send one ([`Authority`], geth `IsAuthorisedAddress`, `consensus/XDPoS/xdpos_api.go:195-212`,
//!   wired at `eth/backend.go:475-484`): in V2 every candidate of the epoch's pool, penalised ones included,
//!   so a penalised node can sign its way back;
//! * it is exempt from the pool minimum tip and the tier floor ([`super::validate`]);
//! * with a known calldata shape ([`known_calldata`]) from such a sender at the pending nonce it goes
//!   straight to pending, whatever the pool fill ([`Pool::try_special_fast_path`], geth
//!   `promoteSpecialTx`, `legacypool.go:793-837`).

use super::{list::Tx, Error, Pool, PoolTx};
use alloy_primitives::Address;
use std::collections::BTreeSet;
use xone_evm::route::{BLOCK_SIGNERS, RANDOMIZE};
use xone_state::StateRead;

/// `common.HexSignMethod`, `common/types.go:66`: `sign(uint256,bytes32)`.
pub const SIGN_SELECTOR: [u8; 4] = [0xe3, 0x41, 0xea, 0xa4];
/// `common.HexSetSecret`, `common/types.go:67`.
pub const SET_SECRET_SELECTOR: [u8; 4] = [0x34, 0xd3, 0x86, 0x00];
/// `common.HexSetOpening`, `common/types.go:68`.
pub const SET_OPENING_SELECTOR: [u8; 4] = [0xe1, 0x1f, 0x5b, 0xa2];
/// Length of a sign transaction's calldata: selector, block number, block hash.
pub const SIGN_CALLDATA_LEN: usize = 68;

/// Who may send a special transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authority {
    /// The node is syncing, or has no pool for the head: nobody is authorised (`eth/backend.go:479`).
    Syncing,
    /// The candidate pool of the head's epoch: V2 `SnapshotV2::candidates` (masternodes, standby and
    /// penalised), V1 the masternodes of the checkpoint.
    Candidates(BTreeSet<Address>),
}

impl Authority {
    pub fn candidates<I: IntoIterator<Item = Address>>(pool: I) -> Self {
        Self::Candidates(pool.into_iter().collect())
    }

    pub fn is_signer(&self, a: &Address) -> bool {
        matches!(self, Self::Candidates(s) if s.contains(a))
    }
}

/// `hasKnownSpecialCalldata`, `legacypool.go:924-939`. 0x89: exactly 68 bytes that start with the sign
/// selector. 0x90: at least 4 bytes that start with the setSecret or setOpening selector.
pub fn known_calldata(tx: &PoolTx) -> bool {
    let (Some(to), d) = (tx.tx.to, tx.tx.data.as_ref()) else {
        return false;
    };
    let Some(sel) = d.get(..4) else {
        return false;
    };
    if to == BLOCK_SIGNERS {
        d.len() == SIGN_CALLDATA_LEN && sel == SIGN_SELECTOR
    } else if to == RANDOMIZE {
        sel == SET_SECRET_SELECTOR || sel == SET_OPENING_SELECTOR
    } else {
        false
    }
}

/// `isSigningTxExpired`, `legacypool.go:860-891`: a sign transaction for block `b` is dead once the head
/// is past `b + 2 * epoch` (`BlockSigner.sol:21`). Anything else, or calldata too short to read, is not.
pub fn signing_tx_expired(tx: &PoolTx, head: u64, epoch: u64) -> bool {
    if tx.tx.to != Some(BLOCK_SIGNERS) || epoch == 0 {
        return false;
    }
    let d = tx.tx.data.as_ref();
    let Some(word) = d.get(4..36).filter(|_| d.len() >= SIGN_CALLDATA_LEN) else {
        return false;
    };
    // big.Int.Uint64 of the 32-byte block number: its low 64 bits.
    let target = word[24..32]
        .iter()
        .fold(0u64, |a, b| (a << 8) | u64::from(*b));
    let expiry = target.wrapping_add(epoch.wrapping_mul(2));
    if expiry < target {
        return false;
    }
    head > expiry
}

impl Pool {
    /// The fast path of `add` (`legacypool.go:960-1015`): `Some(result)` when the transaction is special,
    /// from an authorised sender and of a known calldata shape, and either sits at the pending nonce or
    /// replaces an expired sign transaction. `None`: take the ordinary path.
    pub(crate) fn try_special_fast_path<S: StateRead>(
        &mut self,
        state: &S,
        from: Address,
        tx: &Tx,
    ) -> Option<Result<bool, Error>> {
        if !(tx.is_special() && self.authority.is_signer(&from) && known_calldata(tx)) {
            return None;
        }
        let pending_n = self.nonces.get(state, from);
        if pending_n == tx.tx.nonce {
            return Some(self.promote_special_tx(state, from, tx));
        }
        if pending_n > tx.tx.nonce {
            let expired = self
                .pending
                .get(&from)
                .and_then(|l| l.get(tx.tx.nonce))
                .is_some_and(|old| {
                    signing_tx_expired(old, self.head.number, self.spec.xdpos.epoch)
                });
            if expired {
                return Some(self.promote_special_tx(state, from, tx));
            }
        }
        None
    }

    /// `promoteSpecialTx`, `legacypool.go:793-837`: put the transaction in pending at its nonce, past the
    /// fill limits and the price checks. A special transaction already at that nonce blocks it unless that
    /// one is an expired sign transaction (XDC deviation `#932`).
    fn promote_special_tx<S: StateRead>(
        &mut self,
        state: &S,
        from: Address,
        tx: &Tx,
    ) -> Result<bool, Error> {
        let (epoch, head) = (self.spec.xdpos.epoch, self.head.number);
        let list = self
            .pending
            .entry(from)
            .or_insert_with(|| super::list::List::new(true));
        let old = list.get(tx.tx.nonce).cloned();
        if let Some(o) = &old {
            if o.is_special() && !signing_tx_expired(o, head, epoch) {
                if list.is_empty() {
                    self.pending.remove(&from);
                }
                return Err(Error::DuplicateSpecialTransaction);
            }
        }
        let old = list.put_special(tx);
        if let Some(o) = &old {
            self.all.remove(&o.hash);
            self.priced.removed(1, &self.all);
        }
        if self.all.get(&tx.hash).is_none() {
            self.all.add(tx);
            self.priced.put(tx);
        }
        self.nonces
            .set_if_higher(state, from, tx.tx.nonce.saturating_add(1));
        self.queue_tx_event(tx);
        Ok(true)
    }
}
