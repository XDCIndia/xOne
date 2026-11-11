//! Checkpoint rewards as pure functions of explicit inputs. The driver collects the inputs (the
//! canonical window through [`crate::chain::SignTxCounter`], the masternode, protector and
//! observer sets of the window's opening epoch, the holder data of the parent state) and applies
//! the returned map; nothing here does I/O or keeps state. Rules: `spec/consensus/rewards-*.md`.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, B256, U256};
use xone_chainspec::ChainSpec;

use crate::chain::{Error, SignTx, SignedBlock, MERGE_SIGN_RANGE};

pub mod proportional;
pub mod tiered;
pub mod v1;
pub mod v2;

/// A voter of a masternode and its stake, in contract order.
pub type Voter = (Address, U256);

/// Address to amount for one checkpoint. A zero amount is an entry too: geth credits zero so the
/// account is touched (`consensus/XDPoS/reward.go:640-644`).
pub type RewardMap = BTreeMap<Address, U256>;

/// geth `RewardMasterPercent`, `RewardVoterPercent`, `RewardFoundationPercent`
/// (`consensus/XDPoS/constants.go:26-28`).
const OWNER_PERCENT: u64 = 90;
const VOTER_PERCENT: u64 = 0;
const FOUNDATION_PERCENT: u64 = 10;
/// The package variable `TIP2019Block` of `consensus/XDPoS` (`constants.go`), 1 on every network
/// this crate knows: from block 1 a voter listed twice counts once (`reward.go:351`, `:511`).
const VOTER_DEDUP_FROM: u64 = 1;
/// geth `params.Ether`, `params/denomination.go:26`.
const ETHER: u64 = 1_000_000_000_000_000_000;
/// geth `common.OneYear`, `common/constants.go:20`, and the fallback `common.BlocksPerYear`.
const ONE_YEAR_SECONDS: u64 = 365 * 86_400;
const BLOCKS_PER_YEAR: u64 = 15_768_000;

/// Who gets the reward of a checkpoint (geth `HookReward`, `eth/hooks/engine_v2_hooks.go:60`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// Before the V2 switch block: the V1 window and the flat payout.
    V1,
    /// From the V2 switch block, before the tiered rewards: the epoch-switch window, flat payout.
    V2,
    /// From `TIPFixedReward` or `TIPProportionalReward`: three tiers, fixed or proportional.
    Tiered { proportional: bool },
}

/// The rule of the fork row for block `number` (`engine_v2_hooks.go:93` tiered gate, `:219-233` flat path,
/// `isV2` at `:245-248`).
pub fn rule(spec: &ChainSpec, number: u64) -> Rule {
    let proportional = spec.tip_proportional_reward.is_active(number);
    if spec.tip_fixed_reward.is_active(number) || proportional {
        return Rule::Tiered { proportional };
    }
    match &spec.xdpos.v2 {
        Some(v2) if number >= v2.switch_block => Rule::V2,
        _ => Rule::V1,
    }
}

/// The per-epoch reward window: the canonical blocks from `start` to the block before the
/// checkpoint, and the sets that are paid. Built once, never changed: the fields are private, no
/// method takes `&mut self`, and nothing here has interior mutability, so one `Arc<RewardWindow>`
/// can be read by every thread (xOneGo#700 shared a live map; this holds a frozen copy).
#[derive(Debug, Clone)]
pub struct RewardWindow {
    start: u64,
    end: u64,
    blocks: Vec<SignedBlock>,
    masternodes: Vec<Address>,
    protectors: Vec<Address>,
    observers: Vec<Address>,
}

impl RewardWindow {
    /// `blocks` ascending and contiguous from `start`, the rewarded blocks being `start..=end`.
    pub fn new(
        start: u64,
        end: u64,
        blocks: Vec<SignedBlock>,
        masternodes: Vec<Address>,
        protectors: Vec<Address>,
        observers: Vec<Address>,
    ) -> Result<Self, Error> {
        for (i, b) in blocks.iter().enumerate() {
            let want = u64::try_from(i)
                .ok()
                .and_then(|i| start.checked_add(i))
                .ok_or(Error::Amount)?;
            if b.number != want {
                return Err(Error::NotContiguous {
                    start,
                    number: b.number,
                });
            }
        }
        let have = blocks.last().map_or(start, |b| b.number);
        if end < start || have < end {
            return Err(Error::NotContiguous {
                start,
                number: have,
            });
        }
        Ok(Self {
            start,
            end,
            blocks,
            masternodes,
            protectors,
            observers,
        })
    }

    pub fn start(&self) -> u64 {
        self.start
    }
    pub fn end(&self) -> u64 {
        self.end
    }
    pub fn masternodes(&self) -> &[Address] {
        &self.masternodes
    }

    /// Senders of the sign transactions that sign each rewarded block, in the order of the
    /// window; the key is the hash of the signed block (geth's `data` map).
    fn signers_by_signed_hash(
        &self,
        counted: impl Fn(&SignedBlock, &SignTx) -> bool,
    ) -> BTreeMap<B256, Vec<Address>> {
        let mut data: BTreeMap<B256, Vec<Address>> = BTreeMap::new();
        for b in &self.blocks {
            for tx in b.signs.iter().filter(|tx| counted(b, tx)) {
                data.entry(tx.signed).or_default().push(tx.sender);
            }
        }
        data
    }

    /// geth `GetRewardForCheckpoint` (`reward.go:187-221`) and `GetSigningTxCount`
    /// (`engine_v2_hooks.go:826-860`): per masternode the number of rewarded blocks that it signed,
    /// counting a block once per signer; only every 15th block counts once TIP2019 is active.
    /// `receipts` is the V1 rule that a sign transaction before `TIPSigning` needs a successful
    /// receipt (`reward.go:144`).
    pub fn flat_counts(&self, spec: &ChainSpec, receipts: bool) -> Counts {
        let data = self.signers_by_signed_hash(|b, tx| {
            !receipts || spec.effective.tip_signing.is_active(b.number) || tx.ok
        });
        let mut counts = Counts::default();
        for b in self.blocks.iter().filter(|b| b.number <= self.end) {
            if b.number % MERGE_SIGN_RANGE != 0 && spec.effective.tip2019.is_active(b.number) {
                continue;
            }
            for addr in distinct_members(data.get(&b.hash), &self.masternodes) {
                *counts.signs.entry(addr).or_default() += 1;
                counts.total += 1;
            }
        }
        counts
    }

    /// geth `GetSigningTxCountTiered` (`engine_v2_hooks.go:1022-1043`): the same count for each of
    /// the three tiers, over the blocks whose number is a multiple of 15 only, plus the number of
    /// such blocks (`signSlots`), which the proportional rule divides by.
    pub fn tier_counts(&self) -> TierCounts {
        let data = self.signers_by_signed_hash(|_, _| true);
        let mut t = TierCounts::default();
        for b in self.blocks.iter().filter(|b| b.number <= self.end) {
            if b.number % MERGE_SIGN_RANGE != 0 {
                continue;
            }
            t.slots += 1;
            for (set, tier) in [
                (&self.masternodes, &mut t.masternode),
                (&self.protectors, &mut t.protector),
                (&self.observers, &mut t.observer),
            ] {
                for addr in distinct_members(data.get(&b.hash), set) {
                    *tier.entry(addr).or_default() += 1;
                }
            }
        }
        t
    }
}

/// The members of `set` that are among `senders`, once each.
fn distinct_members(senders: Option<&Vec<Address>>, set: &[Address]) -> BTreeSet<Address> {
    senders
        .into_iter()
        .flatten()
        .filter(|a| set.contains(a))
        .copied()
        .collect()
}

/// Per-signer counts of the flat rule and their sum (`totalSigner`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts {
    pub total: u64,
    pub signs: BTreeMap<Address, u64>,
}

/// Per-tier counts of the tiered rule.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TierCounts {
    pub slots: u64,
    pub masternode: BTreeMap<Address, u64>,
    pub protector: BTreeMap<Address, u64>,
    pub observer: BTreeMap<Address, u64>,
}

/// What the validator contract (0x88) says of the masternodes of a window, read from the state at
/// the START of the checkpoint block, that is the parent's post-state. `root` is that state's root:
/// the engine refuses a snapshot of any other state (geth `Finalize` passes `parentState`,
/// `consensus/XDPoS/xdpos.go:1587`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holders {
    root: B256,
    owners: BTreeMap<Address, Address>,
    voters: BTreeMap<Address, Vec<Voter>>,
}

impl Holders {
    /// `nodes` yields (masternode, owner, voters with stake in contract order). An unset owner is
    /// the zero address: geth does not fall back to the signer (`reward.go:474`).
    pub fn new(
        root: B256,
        nodes: impl IntoIterator<Item = (Address, Address, Vec<Voter>)>,
    ) -> Self {
        let mut owners = BTreeMap::new();
        let mut voters = BTreeMap::new();
        for (node, owner, v) in nodes {
            owners.insert(node, owner);
            voters.insert(node, v);
        }
        Self {
            root,
            owners,
            voters,
        }
    }

    /// Reads the snapshot from a state reader opened on the parent state of root `root`.
    pub fn read(
        root: B256,
        state: &impl xone_state::StateRead,
        nodes: impl IntoIterator<Item = Address>,
    ) -> Self {
        use xone_state::system_contracts::validator_0x88 as v88;
        Self::new(
            root,
            nodes.into_iter().map(|n| {
                let voters = v88::voters(state, n)
                    .into_iter()
                    .map(|v| (v, v88::voter_cap(state, n, v)))
                    .collect();
                (
                    n,
                    v88::candidate_owner(state, n).unwrap_or(Address::ZERO),
                    voters,
                )
            }),
        )
    }

    pub fn root(&self) -> B256 {
        self.root
    }

    fn of(&self, node: Address) -> Result<(Address, &[Voter]), Error> {
        let owner = *self.owners.get(&node).ok_or(Error::NoHolder(node))?;
        let voters = self.voters.get(&node).map_or(&[][..], Vec::as_slice);
        Ok((owner, voters))
    }
}

/// Which `GetRewardBalancesRate` variant pays a signer's reward out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Split {
    /// `reward.go:473`: an owner that is the foundation wallet gets no 90% share (`:499`).
    Flat,
    /// `reward.go:333`: the owner always gets the 90% share (the tiered path, `:341`).
    Upgrade,
}

fn percent(total: U256, p: u64) -> U256 {
    total * U256::from(p) / U256::from(100u64)
}

/// The 90/10 split of one signer's reward into its owner, its voters and the foundation, as
/// geth's per-signer `balances` map.
fn split(
    h: &Holders,
    number: u64,
    foundation: Address,
    node: Address,
    reward: U256,
    mode: Split,
) -> Result<RewardMap, Error> {
    let (owner, voters) = h.of(node)?;
    let mut b = RewardMap::new();
    if mode == Split::Upgrade || owner != foundation {
        b.insert(owner, percent(reward, OWNER_PERCENT));
    }
    if !voters.is_empty() {
        let pool = percent(reward, VOTER_PERCENT);
        let mut caps: BTreeMap<Address, U256> = BTreeMap::new();
        let mut total = U256::ZERO;
        for &(voter, cap) in voters {
            if caps.contains_key(&voter) && VOTER_DEDUP_FROM <= number {
                continue;
            }
            total += cap;
            caps.insert(voter, cap);
        }
        if !total.is_zero() {
            for (addr, cap) in caps.into_iter().filter(|(_, c)| !c.is_zero()) {
                let share = pool * cap / total;
                *b.entry(addr).or_default() += share;
            }
        }
    }
    // An entry that exists gets the foundation share added, one that does not is assigned it
    // (`reward.go:545-549`, and the fork-aware gate at `:378`, always open on the tiered path).
    *b.entry(foundation).or_default() += percent(reward, FOUNDATION_PERCENT);
    Ok(b)
}

/// Pays every signer's reward out to its holders and sums the result per address.
pub(crate) fn pay(
    h: &Holders,
    number: u64,
    parent_root: B256,
    foundation: Address,
    rewards: &BTreeMap<Address, U256>,
    mode: Split,
    into: &mut RewardMap,
) -> Result<(), Error> {
    if h.root != parent_root {
        return Err(Error::WrongState {
            want: parent_root,
            got: h.root,
        });
    }
    for (&node, &reward) in rewards {
        for (addr, amount) in split(h, number, foundation, node, reward, mode)? {
            *into.entry(addr).or_default() += amount;
        }
    }
    Ok(())
}

/// geth `RewardInflation` (`eth/util/util.go:18`): the reward halves at two years and halves
/// again at five years of blocks, until `TIPNoHalvingMNReward`.
pub fn inflation(spec: &ChainSpec, number: u64, reward: U256) -> U256 {
    if spec.effective.tip_no_halving_mn_reward.is_active(number) {
        return reward;
    }
    let bpy = match spec.xdpos.period {
        0 => BLOCKS_PER_YEAR,
        p => ONE_YEAR_SECONDS / p,
    };
    let mut r = reward;
    if bpy.saturating_mul(2) <= number && number < bpy.saturating_mul(5) {
        r /= U256::from(2u64);
    }
    if bpy.saturating_mul(5) <= number {
        r /= U256::from(4u64);
    }
    r
}

/// `floor(reward * 1e18)` as geth computes it for the tier rewards (`engine_v2_hooks.go:133-138`):
/// `big.Float.Mul(SetFloat64(reward), SetUint64(Ether))` rounds the product to 64 mantissa bits,
/// nearest even, and `Int` truncates it. Not the integer product, so a reward such as 63.42 is
/// paid as that float and not as 63.42e18 exactly.
pub fn tier_reward(reward: f64) -> Result<U256, Error> {
    if !reward.is_finite() || reward < 0.0 {
        return Err(Error::Amount);
    }
    let bits = reward.to_bits();
    let frac = bits & ((1u64 << 52) - 1);
    let biased = i32::try_from((bits >> 52) & 0x7ff).map_err(|_| Error::Amount)?;
    let (mant, exp) = if biased == 0 {
        (frac, -1074)
    } else {
        (frac | (1u64 << 52), biased - 1075)
    };
    let mut p = u128::from(mant) * u128::from(ETHER);
    let mut e = exp;
    let len = 128 - p.leading_zeros();
    if len > 64 {
        let shift = len - 64;
        let rem = p & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        p >>= shift;
        if rem > half || (rem == half && p & 1 == 1) {
            p += 1;
        }
        e += i32::try_from(shift).map_err(|_| Error::Amount)?;
    }
    let p = U256::from(p);
    if e >= 0 {
        let s = usize::try_from(e).map_err(|_| Error::Amount)?;
        if s > 100 {
            return Err(Error::Amount);
        }
        Ok(p << s)
    } else {
        let s = usize::try_from(-e).map_err(|_| Error::Amount)?;
        Ok(if s >= 256 { U256::ZERO } else { p >> s })
    }
}
