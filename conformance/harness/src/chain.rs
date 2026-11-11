//! The runner's own chain store: the headers it has verified and executed, the sign transactions
//! of each block as its own execution saw them, the code it deployed, and for V2 the epoch
//! bookkeeping (epoch-switch headers, gap snapshots, the 3-chain commit). Everything the executor
//! reads about earlier blocks comes from here, never from the archive.

use alloy_primitives::{Address, Bytes, B256, U256};
use std::collections::{BTreeMap, HashMap};
use xone_chainspec::ChainSpec;
use xone_consensus::{
    chain::{BlockReader, HeaderView, PenaltyHistory, SignTx},
    v1::{
        checkpoint::{addresses, masternodes_of},
        snapshot::Snapshot,
    },
    v2::{
        commit::{self, Headers, Link},
        epoch_switch,
        gap_snapshot::{self, SnapshotV2},
    },
    verify::{Context, V1Context, V2Context},
};
use xone_executor::Env;
use xone_primitives::{Header, WideBalance};

pub struct Stored {
    pub hash: B256,
    pub header: Header,
    /// The seal signer (V1), the coinbase (V2, it equals the validator), zero for genesis.
    pub miner: Address,
    pub signs: Vec<SignTx>,
    /// geth `XDPoS.IsEpochSwitch`: of a V2 header the round rule (the V2 switch block included), of a
    /// V1 header a multiple of the epoch.
    pub epoch_switch: bool,
    /// V2: the number of the epoch-switch block of this block's epoch. V1: zero.
    pub epoch_start: u64,
}

pub struct Chain {
    pub epoch: u64,
    gap: u64,
    /// `V2.SwitchBlock`, `u64::MAX` when the network has none.
    switch: u64,
    /// Number of `blocks[0]`.
    pub base: u64,
    pub blocks: Vec<Stored>,
    by_hash: HashMap<B256, u64>,
    pub codes: BTreeMap<B256, Bytes>,
    /// Accounts above 2^256 in the state of the tip, kept in step with every committed diff.
    pub wide: BTreeMap<Address, WideBalance>,
    /// Gap snapshots built from the state of V2 gap blocks, by gap block number.
    pub gaps: BTreeMap<u64, SnapshotV2>,
    /// The highest block the 3-chain rule has committed, and how many blocks it has committed.
    pub committed: Option<Link>,
    pub committed_count: u64,
    /// Test mutant: the gap snapshot is read from the gap block this many blocks earlier than the
    /// rule says (0 is the rule).
    pub gap_offset: u64,
    spec: ChainSpec,
}

/// What the store keeps of an executed block.
pub struct Pushed {
    pub hash: B256,
    pub header: Header,
    pub signs: Vec<SignTx>,
}

impl Chain {
    pub fn new(spec: &ChainSpec, base: u64) -> Self {
        Self {
            epoch: spec.xdpos.epoch,
            gap: spec.xdpos.gap,
            switch: spec.xdpos.v2.as_ref().map_or(u64::MAX, |v| v.switch_block),
            base,
            blocks: vec![],
            by_hash: HashMap::new(),
            codes: BTreeMap::new(),
            wide: BTreeMap::new(),
            gaps: BTreeMap::new(),
            committed: None,
            committed_count: 0,
            gap_offset: 0,
            spec: spec.clone(),
        }
    }

    pub fn tip(&self) -> u64 {
        self.base + self.blocks.len() as u64 - 1
    }

    pub fn get(&self, number: u64) -> Option<&Stored> {
        self.blocks
            .get(usize::try_from(number.checked_sub(self.base)?).ok()?)
    }

    pub fn is_v2(&self, number: u64) -> bool {
        number > self.switch
    }

    /// Appends block `p`, which must extend the tip. The miner, the epoch fields and the 3-chain
    /// commit are derived here, from the header alone.
    pub fn push(&mut self, p: Pushed) -> Result<(), String> {
        let n = p.header.number;
        if !self.blocks.is_empty() && n != self.tip() + 1 {
            return Err(format!("block {n} does not extend the tip {}", self.tip()));
        }
        let v2 = self.spec.xdpos.v2.as_ref();
        let (miner, epoch_switch, epoch_start) = match v2 {
            _ if n == 0 => (Address::ZERO, false, 0),
            Some(v) if n > v.switch_block => {
                let sw = epoch_switch::of_header(v, self.epoch, &p.header)
                    .map_err(|e| format!("block {n}: {e}"))?
                    .0;
                let start = if sw {
                    n
                } else {
                    self.get(n - 1)
                        .ok_or(format!("block {n}: the parent is not stored"))?
                        .epoch_start
                };
                (p.header.coinbase, sw, start)
            }
            // geth `XDPoS.IsEpochSwitch` (`consensus/XDPoS/xdpos_api.go:228-235`), which the reward
            // hooks walk with: up to the V2 switch block an epoch switch is a multiple of the epoch.
            Some(v) if n == v.switch_block => {
                (p.header.v1_signer().map_err(|e| e.to_string())?, true, n)
            }
            _ => (
                p.header.v1_signer().map_err(|e| e.to_string())?,
                n.is_multiple_of(self.epoch),
                0,
            ),
        };
        self.by_hash.insert(p.hash, n);
        self.blocks.push(Stored {
            hash: p.hash,
            header: p.header,
            miner,
            signs: p.signs,
            epoch_switch,
            epoch_start,
        });
        if n > self.switch {
            self.track_commit(n)?;
        }
        Ok(())
    }

    /// geth `processQC` and `commitBlocks` (`engine_v2/utils.go:476`, `:530-598`): the QC of block `n`
    /// certifies its parent, and the 3-chain rule says which ancestor that commits.
    fn track_commit(&mut self, n: u64) -> Result<(), String> {
        let parent = self
            .get(n - 1)
            .ok_or(format!("block {n}: the parent is not stored"))?
            .hash;
        let proposed = self
            .link(parent)
            .ok_or(format!("block {n}: no link for the parent"))?;
        let committed =
            commit::commit_blocks(&*self, self.switch, self.committed.as_ref(), &proposed)
                .map_err(|e| format!("block {n}: {e}"))?;
        if let Some(l) = committed {
            self.committed = Some(l);
            self.committed_count += 1;
        }
        Ok(())
    }

    /// Drops the blocks below `keep_from`, which no rule reads any more.
    pub fn prune(&mut self, keep_from: u64) {
        let k = usize::try_from(keep_from.saturating_sub(self.base))
            .unwrap_or(0)
            .min(self.blocks.len());
        for s in self.blocks.drain(..k) {
            self.by_hash.remove(&s.hash);
        }
        self.base += k as u64;
    }

    /// geth `GetMasternodesFromCheckpointHeader` of an epoch-switch or checkpoint header: the V1
    /// list in `extraData` up to the V2 switch block, the `Validators` of a V2 switch.
    fn list_of(&self, number: u64) -> Option<Vec<Address>> {
        let h = &self.get(number)?.header;
        if number <= self.switch {
            masternodes_of(&h.extra_data).ok()
        } else {
            Some(addresses(&h.validators))
        }
    }

    /// The V2 inputs of the header verifier for `header`, whose parent is the tip.
    pub fn v2_context(&self, header: &Header) -> Result<Context, String> {
        let parent = self.get(header.number - 1).ok_or("no parent")?;
        let start = parent.epoch_start;
        let masternodes = self
            .list_of(start)
            .ok_or(format!("no epoch list at {start}"))?;
        let penalties = if start > self.switch {
            addresses(&self.get(start).ok_or("no epoch switch")?.header.penalties)
        } else {
            vec![]
        };
        Ok(Context::V2(V2Context {
            parent_masternodes: (parent.header.number != self.switch).then(|| masternodes.clone()),
            masternodes,
            penalties,
            parent_epoch_switch: start,
        }))
    }

    /// The pool a switch block `number` derives its lists from: the V1 list of the switch block for
    /// the first V2 epoch, else the gap snapshot of its epoch, which the runner built from state.
    pub fn snapshot_for(&self, number: u64) -> Result<SnapshotV2, xone_consensus::v2::Error> {
        use xone_consensus::v2::Error;
        if gap_snapshot::uses_v1_list(number, self.epoch, self.switch)? {
            let h = &self
                .get(self.switch)
                .ok_or(Error::Missing {
                    what: "V2 switch header",
                    number: self.switch,
                })?
                .header;
            return gap_snapshot::bootstrap(&h.extra_data, self.switch);
        }
        let gap = gap_snapshot::gap_number(number, self.epoch, self.gap)?
            .saturating_sub(self.gap_offset);
        self.gaps.get(&gap).cloned().ok_or(Error::Missing {
            what: "gap snapshot (state of the gap block not held)",
            number: gap,
        })
    }

    /// geth `epochSwitch` bookkeeping for the post-block step: a gap block of the V2 era (and of the
    /// switch block) stores the pool read from its own state.
    pub fn is_v2_gap_block(&self, number: u64) -> bool {
        number >= self.switch
            && self.epoch >= self.gap
            && number % self.epoch == self.epoch - self.gap
    }

    /// geth `getEpochSwitchInfoBetween`/`GetPreviousPenaltyByHash` source: the V2 epoch switches seen
    /// by the store, newest last, as (number, validators, penalties).
    pub fn switches(&self) -> impl Iterator<Item = &Stored> {
        self.blocks
            .iter()
            .filter(|s| s.epoch_switch && s.header.number > self.switch)
    }

    /// geth `GetMasternodesWithParents` (`consensus/XDPoS/xdpos.go:2062-2083`): the list of the
    /// epoch of block `x`, from its checkpoint header; `own` is `x`'s header when it is not stored yet.
    fn epoch_list(&self, x: u64, own: &Header) -> Result<Vec<Address>, String> {
        let cp = x - x % self.epoch;
        let h = if cp == x && x == own.number {
            own
        } else {
            &self.get(cp).ok_or(format!("no checkpoint {cp}"))?.header
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
            checkpoint: (cp != n)
                .then(|| self.get(cp).map(|s| s.header.clone()))
                .flatten(),
            // only read where geth runs the full V1 verification, which it does not on Apothem
            checkpoint_signers: vec![],
        })))
    }
}

impl BlockReader for Chain {
    fn header(&self, hash: B256, number: u64) -> Option<HeaderView> {
        let s = self.get(number)?;
        (s.hash == hash).then_some(HeaderView {
            number,
            hash,
            parent_hash: s.header.parent_hash,
            miner: s.miner,
            epoch_switch: s.epoch_switch,
        })
    }
    fn sign_txs(&self, hash: B256, number: u64) -> Option<Vec<SignTx>> {
        let s = self.get(number)?;
        (s.hash == hash).then(|| s.signs.clone())
    }
}

/// geth `GetPreviousPenaltyByHash` (`engine_v2/epochSwitch.go:26-93`): the penalties of the epoch
/// switch `back` epochs before the epoch of `hash`; a walk that reaches the V2 switch block stops
/// there, whose epoch has none.
impl PenaltyHistory for Chain {
    fn penalties(&self, hash: B256, back: usize) -> Option<Vec<Address>> {
        let mut s = self.get(*self.by_hash.get(&hash)?)?;
        if s.header.number <= self.switch {
            return Some(vec![]);
        }
        for _ in 0..back {
            if s.epoch_start <= self.switch + 1 {
                break;
            }
            s = self.get(s.epoch_start - 1)?;
            if s.header.number <= self.switch {
                return Some(vec![]);
            }
        }
        let start = self.get(s.epoch_start)?;
        Some(if start.header.number > self.switch {
            addresses(&start.header.penalties)
        } else {
            vec![]
        })
    }
}

impl Headers for Chain {
    fn link(&self, hash: B256) -> Option<Link> {
        let s = self.get(*self.by_hash.get(&hash)?)?;
        let n = s.header.number;
        let round = if n > self.switch {
            s.header.extra_fields_v2().ok()?.round
        } else {
            0
        };
        Some(Link {
            number: n,
            round,
            hash,
            parent: s.header.parent_hash,
        })
    }
}

impl Env for Chain {
    fn hash_of(&self, number: u64) -> Option<B256> {
        Some(self.get(number)?.hash)
    }
    fn masternodes_at(&self, checkpoint: u64) -> Option<Vec<Address>> {
        self.list_of(checkpoint)
    }
    fn wide_balances(&self) -> Vec<(Address, WideBalance)> {
        self.wide.iter().map(|(a, w)| (*a, w.clone())).collect()
    }
    fn code(&self, hash: B256) -> Option<Bytes> {
        self.codes.get(&hash).cloned()
    }
    fn standby_pool(&self, switch: u64) -> Option<Vec<Address>> {
        let h = &self.get(switch)?.header;
        let pool = self
            .gaps
            .get(&gap_snapshot::gap_number(switch, self.epoch, self.gap).ok()?)?;
        Some(gap_snapshot::standby(
            &pool.candidates,
            &self.list_of(switch)?,
            &addresses(&h.penalties),
        ))
    }
    fn burn(&self, number: u64) -> Option<U256> {
        let h = &self.get(number)?.header;
        Some(
            h.base_fee
                .map_or(U256::ZERO, |f| f * U256::from(h.gas_used)),
        )
    }
}
