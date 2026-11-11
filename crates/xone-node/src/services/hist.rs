//! What the executor and the header verifier read about the blocks below the one they work on, and
//! what the RPC serves of them: the canonical chain with its bodies, receipts, sign transactions,
//! deployed code, V1 snapshots, V2 gap pools and epoch switches. It is the logic of the replay
//! harness's `chain.rs` plus the V2 context, kept by the node itself and never read from a peer.
//!
//! ponytail: memory only, one canonical chain, the parent of a block must be the tip. A block on a
//! side branch is refused by the executor (`InsertError::Exec`), so no reorg ever reaches the
//! store. The MDBX store and a branch-aware reader replace it.

use std::collections::{BTreeMap, HashMap};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
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
use xone_executor::Env;
use xone_primitives::{Header, WideBalance};
use xone_state::{system_contracts::validator_0x88 as v88, StateRead};

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
    pub blocks: Vec<Stored>,
    pub codes: BTreeMap<B256, Bytes>,
    /// Accounts above 2^256 in the state of the tip, kept in step with every committed diff.
    pub wide: BTreeMap<Address, WideBalance>,
    /// V1 snapshots after each block, from the block the node started its memory at.
    pub snaps: BTreeMap<u64, Snapshot>,
    /// V2 candidate pools by gap block number.
    pub gaps: BTreeMap<u64, SnapshotV2>,
    pub switches: Vec<Switch>,
    pub tx_index: HashMap<B256, (u64, u32)>,
    /// Total difficulty of the tip: the rank that peers see in our Status.
    pub td: U256,
}

impl BlockReader for HistInner {
    fn header(&self, hash: B256, number: u64) -> Option<HeaderView> {
        let s = self.blocks.get(usize::try_from(number).ok()?)?;
        (s.hash == hash).then_some(HeaderView {
            number,
            hash,
            parent_hash: s.header.parent_hash,
            miner: s.miner,
            epoch_switch: self.switches.iter().any(|w| w.hash == hash),
        })
    }
    fn sign_txs(&self, hash: B256, number: u64) -> Option<Vec<SignTx>> {
        let s = self.blocks.get(usize::try_from(number).ok()?)?;
        (s.hash == hash).then(|| s.signs.clone())
    }
}

impl PenaltyHistory for HistInner {
    fn penalties(&self, hash: B256, back: usize) -> Option<Vec<Address>> {
        let number = self.blocks.iter().rposition(|b| b.hash == hash)?;
        let at = self
            .switches
            .iter()
            .rposition(|w| usize::try_from(w.number).is_ok_and(|n| n <= number))?;
        let w = &self.switches[at.checked_sub(back)?];
        Some(self.switch_penalties(w.number))
    }
}

impl Env for HistInner {
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
        // the genesis block is always there
        &self.blocks[self.blocks.len() - 1]
    }

    pub fn tip_number(&self) -> u64 {
        self.blocks.len() as u64 - 1
    }

    pub fn by_hash(&self, hash: B256) -> Option<&Stored> {
        // ponytail: linear in the chain; an index by hash comes with the MDBX store
        self.blocks.iter().rev().find(|b| b.hash == hash)
    }

    fn v2_cfg(&self) -> Option<&xone_chainspec::V2> {
        self.spec.xdpos.v2.as_ref()
    }

    /// geth `GetMasternodesWithParents` (`consensus/XDPoS/xdpos.go:2062-2083`): the list of the epoch
    /// of block `x`, from its checkpoint header; `own` is `x`'s header when it is not stored yet.
    fn epoch_list(&self, x: u64, own: &Header) -> Result<Vec<Address>, String> {
        let cp = x - x % self.spec.xdpos.epoch;
        let h = if cp == x && x == own.number {
            own
        } else {
            &self
                .blocks
                .get(usize::try_from(cp).map_err(|e| e.to_string())?)
                .ok_or(format!("no checkpoint {cp}"))?
                .header
        };
        masternodes_of(&h.extra_data).map_err(|e| e.to_string())
    }

    /// The header verifier's V1 inputs for `header`, whose parent is the tip.
    fn v1_context(&self, header: &Header, snap: &Snapshot) -> Result<Context, String> {
        let n = header.number;
        let cp = n - n % self.spec.xdpos.epoch;
        let checkpoint = if cp != n {
            Some(
                self.blocks
                    .get(usize::try_from(cp).map_err(|e| e.to_string())?)
                    .ok_or(format!("no checkpoint {cp}"))?
                    .header
                    .clone(),
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
        let Some(s) = usize::try_from(w.number)
            .ok()
            .and_then(|n| self.blocks.get(n))
        else {
            return vec![];
        };
        if self.v2_cfg().is_some_and(|v| v.switch_block == w.number) {
            masternodes_of(&s.header.extra_data).unwrap_or_default()
        } else {
            addresses(&s.header.validators)
        }
    }

    fn switch_penalties(&self, number: u64) -> Vec<Address> {
        usize::try_from(number)
            .ok()
            .and_then(|n| self.blocks.get(n))
            .map(|s| addresses(&s.header.penalties))
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
            let s = usize::try_from(sw)
                .ok()
                .and_then(|n| self.blocks.get(n))
                .ok_or(v2::Error::Missing {
                    what: "V1 switch block",
                    number: sw,
                })?;
            return gap_snapshot::bootstrap(&s.header.extra_data, sw);
        }
        let g = gap_snapshot::gap_number(number, epoch, gap)?;
        self.gaps.get(&g).cloned().ok_or(v2::Error::Missing {
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

    /// Adds block `s` on the tip, with what its execution produced.
    pub fn push(
        &mut self,
        s: Stored,
        codes: BTreeMap<B256, Bytes>,
        wide: Vec<(Address, Option<WideBalance>)>,
        snap: Option<Snapshot>,
        gap: Option<SnapshotV2>,
    ) {
        let n = s.header.number;
        self.index(&s, n);
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
        self.blocks.push(s);
    }

    /// The transaction index and the epoch switches of a block about to be appended.
    pub fn index(&mut self, s: &Stored, n: u64) {
        for (i, t) in s.body.transactions.iter().enumerate() {
            self.tx_index
                .insert(keccak256(t), (n, u32::try_from(i).unwrap_or(u32::MAX)));
        }
        if let Some(v) = self.spec.xdpos.v2.as_ref().filter(|v| n >= v.switch_block) {
            if let Ok((true, epoch)) = epoch_switch::of_header(v, self.spec.xdpos.epoch, &s.header)
            {
                self.switches.push(Switch {
                    number: n,
                    hash: s.hash,
                    round: self.round_of(&s.header),
                    epoch,
                });
            }
        }
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
