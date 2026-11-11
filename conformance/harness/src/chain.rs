//! The runner's own chain store: the headers it has verified and executed, the sign transactions
//! of each block as its own execution saw them, and the code it deployed. Everything the
//! executor reads about earlier blocks comes from here, never from the archive.

use alloy_primitives::{Address, Bytes, B256};
use std::collections::BTreeMap;
use xone_consensus::{
    chain::{BlockReader, HeaderView, SignTx},
    v1::{checkpoint::masternodes_of, snapshot::Snapshot},
    verify::{Context, V1Context},
};
use xone_executor::Env;
use xone_primitives::{Header, WideBalance};

pub struct Stored {
    pub hash: B256,
    pub header: Header,
    /// The seal signer (V1), zero for the genesis block.
    pub miner: Address,
    pub signs: Vec<SignTx>,
}

pub struct Chain {
    pub epoch: u64,
    pub blocks: Vec<Stored>,
    pub codes: BTreeMap<B256, Bytes>,
    /// Accounts above 2^256 in the state of the tip, kept in step with every committed diff.
    pub wide: BTreeMap<Address, WideBalance>,
}

impl BlockReader for Chain {
    fn header(&self, hash: B256, number: u64) -> Option<HeaderView> {
        let s = self.blocks.get(usize::try_from(number).ok()?)?;
        (s.hash == hash).then_some(HeaderView {
            number,
            hash,
            parent_hash: s.header.parent_hash,
            miner: s.miner,
            epoch_switch: false,
        })
    }
    fn sign_txs(&self, hash: B256, number: u64) -> Option<Vec<SignTx>> {
        let s = self.blocks.get(usize::try_from(number).ok()?)?;
        (s.hash == hash).then(|| s.signs.clone())
    }
}

impl Env for Chain {
    fn hash_of(&self, number: u64) -> Option<B256> {
        Some(self.blocks.get(usize::try_from(number).ok()?)?.hash)
    }
    fn masternodes_at(&self, checkpoint: u64) -> Option<Vec<Address>> {
        let s = self.blocks.get(usize::try_from(checkpoint).ok()?)?;
        masternodes_of(&s.header.extra_data).ok()
    }
    fn wide_balances(&self) -> Vec<(Address, WideBalance)> {
        self.wide.iter().map(|(a, w)| (*a, w.clone())).collect()
    }
    fn code(&self, hash: B256) -> Option<Bytes> {
        self.codes.get(&hash).cloned()
    }
}

impl Chain {
    pub fn tip(&self) -> u64 {
        self.blocks.len() as u64 - 1
    }

    /// geth `GetMasternodesWithParents` (`consensus/XDPoS/xdpos.go:2062-2083`): the list of the
    /// epoch of block `x`, from its checkpoint header; `own` is `x`'s header when it is not stored yet.
    fn epoch_list(&self, x: u64, own: &Header) -> Result<Vec<Address>, String> {
        let cp = x - x % self.epoch;
        let h = if cp == x && x == own.number {
            own
        } else {
            &self
                .blocks
                .get(cp as usize)
                .ok_or(format!("no checkpoint {cp}"))?
                .header
        };
        masternodes_of(&h.extra_data).map_err(|e| e.to_string())
    }

    /// The header verifier's V1 inputs for `header`, whose parent is the tip: the epoch lists, the
    /// signers of the snapshot at the parent, and the checkpoint header of the epoch.
    pub fn v1_context(&self, header: &Header, snap: &Snapshot) -> Result<Context, String> {
        let n = header.number;
        let cp = n - n % self.epoch;
        Ok(Context::V1(Box::new(V1Context {
            masternodes: self.epoch_list(n, header)?,
            parent_masternodes: self.epoch_list(n - 1, header)?,
            signers: snap.get_signers(),
            checkpoint: (cp != n).then(|| self.blocks[cp as usize].header.clone()),
            // only read where geth runs the full V1 verification, which it does not on Apothem
            checkpoint_signers: vec![],
        })))
    }
}
