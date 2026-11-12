//! What the executor and the header verifier read about the blocks below the one they work on, and
//! what the RPC serves of them: the canonical chain with its bodies, receipts, sign transactions,
//! deployed code, V1 snapshots, V2 gap pools and epoch switches. It is the logic of the replay
//! harness's `chain.rs` plus the V2 context, kept by the node itself and never read from a peer.
//!
//! Memory is bounded by a window, not by the chain: headers, seals and sign transactions of the last
//! [`window_of`] blocks (eight epochs, above the deepest consensus read), bodies and receipts of the
//! last [`MEMORY_WINDOW`]. Below the window a header, a canonical hash or a number is read from the
//! store ([`Src`], without the store's lock), a transaction location from `TxLookup`, an epoch switch
//! from `ConsensusEpoch` (all of them are also in memory, 56 bytes an epoch) and a gap pool from
//! `ConsensusSnapshot`. Sign transactions are kept for the window only: a read below it is `None`,
//! which the consensus code turns into its typed `Missing` error, never into an empty list.
//!
//! ponytail: one canonical chain, the parent of a block must be the tip. A block on a side branch is
//! refused by the executor (`InsertError::Exec`), so no reorg ever reaches the store. A
//! branch-aware reader replaces it.

use std::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
};

use alloy_primitives::{Address, Bytes, B256, U256};
use xone_chainspec::{ChainSpec, ForkBlock};
use xone_consensus::{
    chain::{BlockReader, HeaderView, PenaltyHistory, SignTx},
    v1::{
        checkpoint::{self, addresses, masternodes_of},
        snapshot::Snapshot,
    },
    v2::{
        self,
        epoch_switch::{self, SwitchQuery},
        gap_snapshot::{self, SnapshotV2},
    },
    verify::{Body, Context, HeaderError, V1Context, V2Context},
};
use xone_db::{mdbx::T, segments::OsDir};
use xone_executor::Env;
use xone_primitives::{Header, WideBalance};
use xone_state::{system_contracts::validator_0x88 as v88, StateRead};
use xone_sync::mdbx::StoreReader;

/// The store read below the window.
pub type Src = StoreReader<OsDir>;

/// Blocks at the tip whose bodies and receipts the history keeps in memory, as geth's `TriesInMemory`
/// (`core/state/statedb.go:47`): on a chain of 10,000-transaction blocks each one costs megabytes.
/// Older bodies and receipts are read from the store (`Core::records`).
pub const MEMORY_WINDOW: u64 = 128;

/// Epochs of headers and sign transactions kept in memory. The deepest consensus read is five epoch
/// switches back (the V2 penalty of `TIPUpgradePenalty` with Apothem's `limitPenaltyEpoch` 5) or five
/// epochs of blocks (the V1 penalty, `LIMIT_PENALTY_EPOCH + 1`); an epoch holds at most `epoch`
/// blocks (`xone_sync::bootstrap::retention`). `tests::window_covers_every_rule` checks it against
/// the derived retention of every network.
pub const EPOCHS_IN_MEMORY: u64 = 8;

/// Blocks whose headers and sign transactions the history keeps in memory.
pub fn window_of(spec: &ChainSpec) -> u64 {
    (EPOCHS_IN_MEMORY * spec.xdpos.epoch).max(MEMORY_WINDOW)
}

/// `ConsensusEpoch` row of a V2 epoch switch: hash, round u64 BE, epoch u64 BE.
pub fn switch_row(w: &Switch) -> Vec<u8> {
    [&w.hash[..], &w.round.to_be_bytes(), &w.epoch.to_be_bytes()].concat()
}

pub fn switch_of_row(number: u64, b: &[u8]) -> Option<Switch> {
    Some(Switch {
        number,
        hash: B256::from_slice(b.get(..32)?),
        round: u64::from_be_bytes(b.get(32..40)?.try_into().ok()?),
        epoch: u64::from_be_bytes(b.get(40..48)?.try_into().ok()?),
    })
}

/// `ConsensusSnapshot` row of a V2 gap pool: the candidates, 20 bytes each.
pub fn gap_row(g: &SnapshotV2) -> Vec<u8> {
    g.candidates.iter().flat_map(|a| a.0 .0).collect()
}

fn gap_of_row(number: u64, b: &[u8]) -> Option<SnapshotV2> {
    b.len().is_multiple_of(20).then(|| SnapshotV2 {
        number,
        candidates: b.chunks(20).map(Address::from_slice).collect(),
    })
}

pub struct Stored {
    pub hash: B256,
    pub header: Header,
    /// The seal signer (V1) or the coinbase (V2); zero for the genesis block.
    pub miner: Address,
    pub signs: Vec<SignTx>,
    pub body: Body,
    pub receipts: Vec<Bytes>,
}

/// An epoch switch block of V2 (the V1 switch block included, it opens the first V2 epoch).
#[derive(Debug, Clone)]
pub struct Switch {
    pub number: u64,
    pub hash: B256,
    pub round: u64,
    pub epoch: u64,
}

pub struct HistInner {
    pub spec: ChainSpec,
    /// The canonical blocks from number `base` to the tip, consecutive, at most `window` of them.
    pub base: u64,
    pub blocks: VecDeque<Stored>,
    /// [`window_of`] the spec; a test may set a smaller one.
    pub window: u64,
    /// The store, for what is below the window. `None` in a history that is not over a store.
    pub src: Option<Src>,
    pub codes: BTreeMap<B256, Bytes>,
    /// Accounts above 2^256 in the state of the tip, kept in step with every committed diff.
    pub wide: BTreeMap<Address, WideBalance>,
    /// V1 snapshots after each of the last [`MEMORY_WINDOW`] blocks.
    pub snaps: BTreeMap<u64, Snapshot>,
    /// V2 candidate pools by gap block number, inside the window; every one is in `ConsensusSnapshot`.
    pub gaps: BTreeMap<u64, SnapshotV2>,
    /// Every V2 epoch switch of the chain (`ConsensusEpoch`).
    pub switches: Vec<Switch>,
    /// Total difficulty of the tip: the rank that peers see in our Status.
    pub td: U256,
}

/// geth `XDPoS.IsEpochSwitch` (`consensus/XDPoS/xdpos_api.go:228-235`) at or below the V2 switch block, which
/// the V2 reward walks with (`eth/hooks/engine_v2_hooks.go:688-718`): a multiple of the epoch, **the genesis
/// block included** (`0 % Epoch == 0`). On a chain whose V2 switch is at its first checkpoint the walk of the
/// first V2 reward ends there: the window is `1..=switch - 1` and its masternodes are the genesis list.
fn v1_epoch_switch(number: u64, switch_block: u64, epoch: u64) -> bool {
    number <= switch_block && number.is_multiple_of(epoch)
}

impl BlockReader for HistInner {
    fn header(&self, hash: B256, number: u64) -> Option<HeaderView> {
        let (parent_hash, miner) = match self.at(number) {
            Some(s) => (s.hash == hash).then_some((s.header.parent_hash, s.miner))?,
            None => {
                let h = self.header_at(number)?;
                (h.hash() == hash).then_some(())?;
                (h.parent_hash, super::core::miner_of(&self.spec, &h).ok()?)
            }
        };
        Some(HeaderView {
            number,
            hash,
            parent_hash,
            miner,
            epoch_switch: self.switches.iter().any(|w| w.hash == hash)
                || self.v2_cfg().is_some_and(|v| {
                    v1_epoch_switch(number, v.switch_block, self.spec.xdpos.epoch)
                }),
        })
    }
    fn sign_txs(&self, hash: B256, number: u64) -> Option<Vec<SignTx>> {
        let s = self.at(number)?;
        (s.hash == hash).then(|| s.signs.clone())
    }
}

impl PenaltyHistory for HistInner {
    fn penalties(&self, hash: B256, back: usize) -> Option<Vec<Address>> {
        let number = self.number_of(hash)?;
        let at = self.switches.iter().rposition(|w| w.number <= number)?;
        let w = &self.switches[at.checked_sub(back)?];
        Some(self.switch_penalties(w.number))
    }
}

impl Env for HistInner {
    fn hash_of(&self, number: u64) -> Option<B256> {
        self.hash_at(number)
    }
    fn masternodes_at(&self, checkpoint: u64) -> Option<Vec<Address>> {
        self.list_at(checkpoint)
    }
    fn wide_balances(&self) -> Vec<(Address, WideBalance)> {
        self.wide.iter().map(|(a, w)| (*a, w.clone())).collect()
    }
    fn code(&self, hash: B256) -> Option<Bytes> {
        self.codes.get(&hash).cloned()
    }
    /// The standby candidates of the epoch that switch block `switch` opens: its gap pool less the
    /// switch's masternodes and penalties (the replay harness's `Chain::standby_pool`).
    fn standby_pool(&self, switch: u64) -> Option<Vec<Address>> {
        let h = self.header_at(switch)?;
        let x = &self.spec.xdpos;
        let pool = self.gap(gap_snapshot::gap_number(switch, x.epoch, x.gap).ok()?)?;
        Some(gap_snapshot::standby(
            &pool.candidates,
            &self.list_at(switch)?,
            &addresses(&h.penalties),
        ))
    }
    /// The base fee burnt by block `number`: base fee times gas used.
    fn burn(&self, number: u64) -> Option<alloy_primitives::U256> {
        let h = self.header_at(number)?;
        Some(
            h.base_fee
                .map_or(U256::ZERO, |f| f * U256::from(h.gas_used)),
        )
    }
}

fn at_gap(spec: &ChainSpec, n: u64) -> bool {
    let x = &spec.xdpos;
    x.gap != 0 && x.gap <= x.epoch && n % x.epoch == x.epoch - x.gap
}

/// geth `updateM1At` (`core/blockchain.go:3963-3990`) and `UpdateMasternodesV1`
/// (`consensus/XDPoS/xdpos.go:2135-2150`, `:2156-2174`): at a V1 gap block the snapshot's signers
/// become the cap-sorted candidates of 0x88 in that block's state.
pub fn reseed(
    spec: &ChainSpec,
    snap: &mut Snapshot,
    state: &impl StateRead,
    n: u64,
) -> Result<(), String> {
    let x = &spec.xdpos;
    let v1 = x.v2.as_ref().is_none_or(|v| n <= v.switch_block);
    if !(at_gap(spec, n) && v1) {
        return Ok(());
    }
    let cands: Vec<_> = v88::candidates(state)
        .into_iter()
        .map(|a| (a, v88::candidate_cap(state, a)))
        .collect();
    let tip = match spec.tip_increase_masternodes {
        ForkBlock::At(b) => Some(b),
        _ => None,
    };
    let max = checkpoint::max_masternodes(
        n,
        tip,
        x.v2.as_ref().map(|v| v.switch_block),
        x.max_masternodes_v2,
    );
    snap.reseed(checkpoint::select_candidates(&cands, max).map_err(|e| e.to_string())?);
    Ok(())
}

impl HistInner {
    pub fn tip(&self) -> &Stored {
        // the window always holds the tip
        &self.blocks[self.blocks.len() - 1]
    }

    pub fn tip_number(&self) -> u64 {
        self.base + self.blocks.len() as u64 - 1
    }

    /// Block `number` of the window, `None` below it or above the tip.
    pub fn at(&self, number: u64) -> Option<&Stored> {
        self.blocks
            .get(usize::try_from(number.checked_sub(self.base)?).ok()?)
    }

    /// The canonical header `number`: from the window, else from the store.
    pub fn header_at(&self, number: u64) -> Option<Cow<'_, Header>> {
        if let Some(s) = self.at(number) {
            return Some(Cow::Borrowed(&s.header));
        }
        if number > self.tip_number() {
            return None;
        }
        Some(Cow::Owned(self.src.as_ref()?.header(number).ok()??))
    }

    /// The canonical hash of block `number`: from the window, else from the store.
    pub fn hash_at(&self, number: u64) -> Option<B256> {
        if let Some(s) = self.at(number) {
            return Some(s.hash);
        }
        if number > self.tip_number() {
            return None;
        }
        self.src.as_ref()?.hash(number).ok()?
    }

    /// The number of canonical block `hash`: from the window, else from the store.
    pub fn number_of(&self, hash: B256) -> Option<u64> {
        if let Some(s) = self.by_hash(hash) {
            return Some(s.header.number);
        }
        self.src
            .as_ref()?
            .number(hash)
            .ok()?
            .filter(|n| *n <= self.tip_number())
    }

    /// The canonical header `hash`.
    pub fn header_by_hash(&self, hash: B256) -> Option<Cow<'_, Header>> {
        let h = self.header_at(self.number_of(hash)?)?;
        (h.hash() == hash).then_some(h)
    }

    /// Block number and index of canonical transaction `hash` (`TxLookup`).
    pub fn tx(&self, hash: B256) -> Option<(u64, u32)> {
        self.src
            .as_ref()?
            .tx(hash)
            .ok()?
            .filter(|(n, _)| *n <= self.tip_number())
    }

    /// The gap pool of gap block `number`: from the window, else from `ConsensusSnapshot`.
    fn gap(&self, number: u64) -> Option<Cow<'_, SnapshotV2>> {
        if let Some(g) = self.gaps.get(&number) {
            return Some(Cow::Borrowed(g));
        }
        let row = self
            .src
            .as_ref()?
            .row(T::ConsensusSnapshot, number)
            .ok()??;
        gap_of_row(number, &row).map(Cow::Owned)
    }

    /// Minimal mode: drops what the store dropped. Headers below `headers_from` go, bodies and
    /// receipts below `records_from` are cleared (the RPC refuses them by class, `Held`), the V1
    /// snapshots and the transaction index follow, gap pools below `gaps_from` go, and so do the
    /// switches below the last one at or under `headers_from` (the epoch of the first header).
    pub fn prune(&mut self, headers_from: u64, records_from: u64, gaps_from: u64) {
        let drop = usize::try_from(headers_from.saturating_sub(self.base)).unwrap_or(usize::MAX);
        let drop = drop.min(self.blocks.len().saturating_sub(1));
        self.blocks.drain(..drop);
        self.base += drop as u64;
        let base = self.base;
        for b in self
            .blocks
            .iter_mut()
            .take_while(|b| b.header.number < records_from)
        {
            b.body = Body::default();
            b.receipts.clear();
            b.signs.clear();
        }
        self.snaps = self.snaps.split_off(&base);
        self.gaps = self.gaps.split_off(&gaps_from);
        let keep = self
            .switches
            .iter()
            .rposition(|w| w.number <= base)
            .unwrap_or(0);
        self.switches.drain(..keep);
    }

    /// Block `hash` of the window.
    pub fn by_hash(&self, hash: B256) -> Option<&Stored> {
        // ponytail: linear in the window (eight epochs); below it [`HistInner::number_of`] reads
        // `HeaderNumbers`
        self.blocks.iter().rev().find(|b| b.hash == hash)
    }

    /// The masternode list a checkpoint or epoch-switch header carries (geth
    /// `GetMasternodesFromEpochSwitchHeader`, `engine_v2/engine.go:1656-1705`): `extraData` up to and
    /// including the V2 switch block, `validators` above it.
    fn list_at(&self, number: u64) -> Option<Vec<Address>> {
        let h = self.header_at(number)?;
        if self.v2_cfg().is_none_or(|v| number <= v.switch_block) {
            masternodes_of(&h.extra_data).ok()
        } else {
            Some(addresses(&h.validators))
        }
    }

    fn v2_cfg(&self) -> Option<&xone_chainspec::V2> {
        self.spec.xdpos.v2.as_ref()
    }

    /// geth `GetMasternodesWithParents` (`consensus/XDPoS/xdpos.go:2062-2083`): the list of the epoch
    /// of block `x`, from its checkpoint header; `own` is `x`'s header when it is not stored yet.
    fn epoch_list(&self, x: u64, own: &Header) -> Result<Vec<Address>, String> {
        let cp = x - x % self.spec.xdpos.epoch;
        let h = if cp == x && x == own.number {
            Cow::Borrowed(own)
        } else {
            self.header_at(cp).ok_or(format!("no checkpoint {cp}"))?
        };
        masternodes_of(&h.extra_data).map_err(|e| e.to_string())
    }

    /// The header verifier's V1 inputs for `header`, whose parent is the tip.
    fn v1_context(&self, header: &Header, snap: &Snapshot) -> Result<Context, String> {
        let n = header.number;
        let cp = n - n % self.spec.xdpos.epoch;
        let checkpoint = if cp != n {
            Some(
                self.header_at(cp)
                    .ok_or(format!("no checkpoint {cp}"))?
                    .into_owned(),
            )
        } else {
            None
        };
        Ok(Context::V1(Box::new(V1Context {
            masternodes: self.epoch_list(n, header)?,
            parent_masternodes: self.epoch_list(n - 1, header)?,
            signers: snap.get_signers(),
            checkpoint,
            // geth runs the full V1 verification off Apothem (`xdpos.go:1210`, `:1371`)
            checkpoint_signers: if self.spec.chain_id == 51 {
                vec![]
            } else {
                snap.get_signers()
            },
        })))
    }

    fn switch_list(&self, w: &Switch) -> Vec<Address> {
        let Some(h) = self.header_at(w.number) else {
            return vec![];
        };
        if self.v2_cfg().is_some_and(|v| v.switch_block == w.number) {
            masternodes_of(&h.extra_data).unwrap_or_default()
        } else {
            addresses(&h.validators)
        }
    }

    fn switch_penalties(&self, number: u64) -> Vec<Address> {
        self.header_at(number)
            .map(|h| addresses(&h.penalties))
            .unwrap_or_default()
    }

    /// The epoch switch of the epoch that holds block `number` of the tip's chain.
    pub fn switch_of(&self, number: u64) -> Option<&Switch> {
        self.switches.iter().rev().find(|w| w.number <= number)
    }

    fn round_of(&self, h: &Header) -> u64 {
        h.extra_fields_v2().map_or(0, |f| f.round)
    }

    /// The pool of the epoch whose switch block is `number` (geth `getSnapshot`).
    pub fn pool(&self, number: u64) -> Result<SnapshotV2, v2::Error> {
        let (epoch, gap) = (self.spec.xdpos.epoch, self.spec.xdpos.gap);
        let sw = self.v2_cfg().ok_or(v2::Error::NoV2)?.switch_block;
        if gap_snapshot::uses_v1_list(number, epoch, sw)? {
            let h = self.header_at(sw).ok_or(v2::Error::Missing {
                what: "V1 switch block",
                number: sw,
            })?;
            return gap_snapshot::bootstrap(&h.extra_data, sw);
        }
        let g = gap_snapshot::gap_number(number, epoch, gap)?;
        self.gap(g).map(Cow::into_owned).ok_or(v2::Error::Missing {
            what: "gap snapshot",
            number: g,
        })
    }

    /// The V2 inputs for `header` on the tip: the lists of its epoch, or at a switch the lists derived
    /// from the gap pool and the penalty engine, never read from the header being checked.
    fn v2_context(&self, header: &Header, parent: &Header) -> Result<Context, HeaderError> {
        let v2c = self.v2_cfg().ok_or(HeaderError::NoV2Section)?;
        let derr = HeaderError::SwitchDerivation;
        let epoch = self.spec.xdpos.epoch;
        let psw = self
            .switch_of(parent.number)
            .ok_or(derr(v2::Error::Missing {
                what: "epoch switch of the parent",
                number: parent.number,
            }))?;
        let plist = self.switch_list(psw);
        let parent_masternodes = (parent.number != v2c.switch_block).then(|| plist.clone());
        let (is_switch, _) = epoch_switch::of_header(v2c, epoch, header).map_err(derr)?;
        let (masternodes, penalties) = if is_switch {
            let q = SwitchQuery {
                number: header.number,
                parent: header.parent_hash,
                round: self.round_of(header),
                parent_round: self.round_of(parent),
                parent_masternodes: plist,
            };
            let pool = self.pool(header.number).map_err(derr)?;
            let e = epoch_switch::expected(&self.spec, self, &q, &pool).map_err(derr)?;
            (e.validators, e.penalties)
        } else {
            (plist, self.switch_penalties(psw.number))
        };
        Ok(Context::V2(V2Context {
            masternodes,
            penalties,
            parent_masternodes,
            parent_epoch_switch: psw.number,
        }))
    }

    /// The verifier context of `header` whose parent is the tip, V1 or V2 by its number.
    pub fn context(
        &self,
        header: &Header,
        parent: &Header,
        snap: Option<&Snapshot>,
    ) -> Result<Context, HeaderError> {
        let n = header.number;
        if self.v2_cfg().is_some_and(|v| n > v.switch_block) {
            return self.v2_context(header, parent);
        }
        let snap = snap.ok_or(HeaderError::UnknownAncestor(n - 1))?;
        self.v1_context(header, snap)
            .map_err(|_| HeaderError::UnknownAncestor(n - 1))
    }

    /// Adds block `s` on the tip, with what its execution produced. Returns the epoch switch it is,
    /// if it is one (the caller stores it with the gap pool).
    pub fn push(
        &mut self,
        s: Stored,
        codes: BTreeMap<B256, Bytes>,
        wide: Vec<(Address, Option<WideBalance>)>,
        snap: Option<Snapshot>,
        gap: Option<SnapshotV2>,
    ) -> Option<Switch> {
        let n = s.header.number;
        let switch = self.index(&s, n);
        self.td += s.header.difficulty;
        self.codes.extend(codes);
        for (a, w) in wide {
            match w {
                Some(w) => self.wide.insert(a, w),
                None => self.wide.remove(&a),
            };
        }
        if let Some(sn) = snap {
            self.snaps.insert(n, sn);
        }
        if let Some(g) = gap {
            self.gaps.insert(g.number, g);
        }
        self.blocks.push_back(s);
        // the body and receipts that leave the memory window are read from the store from now on
        if let Some(old) = n
            .checked_sub(MEMORY_WINDOW)
            .and_then(|o| usize::try_from(o.checked_sub(self.base)?).ok())
            .and_then(|i| self.blocks.get_mut(i))
        {
            old.body = Body::default();
            old.receipts = Vec::new();
        }
        self.trim();
        switch
    }

    /// Drops what has left the windows: blocks below `window`, snapshots below [`MEMORY_WINDOW`],
    /// gap pools below `window` (they are in `ConsensusSnapshot`).
    pub fn trim(&mut self) {
        while self.blocks.len() as u64 > self.window.max(1) {
            self.blocks.pop_front();
            self.base += 1;
        }
        let tip = self.tip_number();
        let snaps_from = tip.saturating_sub(MEMORY_WINDOW);
        while self
            .snaps
            .first_key_value()
            .is_some_and(|(k, _)| *k < snaps_from)
        {
            self.snaps.pop_first();
        }
        let gaps_from = tip.saturating_sub(self.window);
        while self
            .gaps
            .first_key_value()
            .is_some_and(|(k, _)| *k < gaps_from)
        {
            self.gaps.pop_first();
        }
    }

    /// True when block `n`'s body and receipts are in memory; below the window they are in the store
    /// only ([`MEMORY_WINDOW`]).
    pub fn in_memory(&self, n: u64) -> bool {
        n + MEMORY_WINDOW > self.tip_number()
    }

    /// The epoch switch of a block about to be appended, added to `switches`.
    pub fn index(&mut self, s: &Stored, n: u64) -> Option<Switch> {
        let v = self
            .spec
            .xdpos
            .v2
            .as_ref()
            .filter(|v| n >= v.switch_block)?;
        let (true, epoch) = epoch_switch::of_header(v, self.spec.xdpos.epoch, &s.header).ok()?
        else {
            return None;
        };
        let w = Switch {
            number: n,
            hash: s.hash,
            round: self.round_of(&s.header),
            epoch,
        };
        self.switches.push(w.clone());
        Some(w)
    }

    pub fn is_gap_block(&self, n: u64) -> bool {
        self.v2_cfg().is_some_and(|v| n > v.switch_block) && at_gap(&self.spec, n)
    }

    pub fn switch_masternodes(&self, w: &Switch) -> Vec<Address> {
        self.switch_list(w)
    }

    pub fn switch_pen(&self, w: &Switch) -> Vec<Address> {
        self.switch_penalties(w.number)
    }
}

#[cfg(test)]
mod tests {
    use xone_chainspec::Network;
    use xone_sync::bootstrap::{Class, Retention};

    use super::*;

    /// The window holds every header, body and receipt a consensus rule reads below the block it runs
    /// on, on every network, at genesis, around the V2 switch and far above it.
    #[test]
    fn window_covers_every_rule() {
        for net in Network::ALL {
            let spec = net.spec();
            let w = window_of(&spec);
            let sw = spec.xdpos.v2.as_ref().map_or(0, |v| v.switch_block);
            let e = spec.xdpos.epoch;
            for pivot in [
                e,
                sw.saturating_sub(e),
                sw,
                sw + 5 * e,
                sw + 100_000,
                100_000_000,
            ] {
                let r = Retention::derive(&spec, pivot).unwrap();
                for c in [Class::Header, Class::Body, Class::Receipt] {
                    // the window counts the block being imported, the retention its pivot
                    assert!(
                        r.window(c).0 < w,
                        "{net:?} {pivot} {c:?} {:?} {w}",
                        r.window(c)
                    );
                }
            }
        }
    }

    #[test]
    fn rows_round_trip() {
        let w = Switch {
            number: 7,
            hash: B256::repeat_byte(3),
            round: 1 << 40,
            epoch: 9,
        };
        let back = switch_of_row(7, &switch_row(&w)).unwrap();
        assert_eq!(
            (back.hash, back.round, back.epoch),
            (w.hash, w.round, w.epoch)
        );
        let g = SnapshotV2 {
            number: 5,
            candidates: vec![Address::repeat_byte(1), Address::repeat_byte(2)],
        };
        assert_eq!(
            gap_of_row(5, &gap_row(&g)).unwrap().candidates,
            g.candidates
        );
        assert!(gap_of_row(5, &[0; 21]).is_none());
    }

    /// The first V2 reward of a chain that switches at its first checkpoint (epoch 30, switch 30) walks down
    /// to the genesis block; geth counts it as an epoch switch, and a reader that did not left the reward
    /// with no window (`no second epoch switch below block 60`, the gate D devnet).
    #[test]
    fn genesis_is_a_v1_epoch_switch() {
        assert!(v1_epoch_switch(0, 30, 30));
        assert!(v1_epoch_switch(30, 30, 30));
        assert!(!v1_epoch_switch(15, 30, 30));
        assert!(
            !v1_epoch_switch(60, 30, 30),
            "above the switch the V2 rule decides"
        );
        assert!(v1_epoch_switch(900, 2700, 900));
    }
}
