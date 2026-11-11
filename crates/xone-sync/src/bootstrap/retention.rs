//! How far below a block every rule reads, derived from the chain spec, and the typed refusal for
//! history that a pivoted node does not hold. The table with the source line of every row is
//! `spec/retention.md`; the arms below carry the arithmetic.
//!
//! A lookback is a number of blocks below the block `n` the rule runs on: the rule reads block
//! `n - lookback` and everything above it up to `n`. `E` is `XDPoS.epoch`, `G` is `XDPoS.gap`.
//! Epochs are counted in rounds and a round makes at most one block, so an epoch holds at most `E`
//! blocks and the walk to the `k`-th epoch switch below `n` is at most `k * E` blocks. On the
//! Apothem archive the spacing of 2,532 consecutive switches is 12 to 900 blocks, the two-switch
//! span is at most 1,800 and the five-switch span is exactly 4,500 (`tests/bootstrap/`).

use std::collections::BTreeMap;

use xone_chainspec::{ChainSpec, V2Config, V2};
use xone_consensus::chain::RANGE_RETURN_SIGNER;
use xone_consensus::penalty::{v1::LIMIT_PENALTY_EPOCH, v2::post_upgrade_limit};
use xone_consensus::reward::{self, v1::checkpoint_period, Rule as Pay};

/// A kind of data a rule reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Header,
    Body,
    Receipt,
    /// The candidate pool (`SnapshotV2`, or the V1 signer snapshot) stored at a gap block. It is
    /// derived from the 0x88 state at that block, which a node that pivoted above it does not have.
    GapSnapshot,
    /// The state of the pivot block. Nothing below it is kept.
    State,
}

/// A rule that reads history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    V1Reward,
    V2Reward,
    TieredReward,
    V1Penalty,
    V2PenaltyPre,
    V2PenaltyPost,
    GapPool,
    V1Snapshot,
    QcEpoch,
    ThreeChain,
    TcWalk,
    RpcMasternodes,
    RpcMissedRounds,
    RpcGetLogs,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Missing {
    #[error(
        "{class:?} of block {block} is below the retained history (first retained {floor}, pivot {pivot})"
    )]
    UnsupportedHistory {
        class: Class,
        block: u64,
        floor: u64,
        pivot: u64,
    },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("the epoch is zero")]
    ZeroEpoch,
    #[error("block number out of range")]
    Overflow,
    #[error(transparent)]
    Missing(#[from] Missing),
}

impl Rule {
    /// The rules a block triggers by itself. They set the retention.
    pub const CONSENSUS: [Rule; 11] = [
        Rule::V1Reward,
        Rule::V2Reward,
        Rule::TieredReward,
        Rule::V1Penalty,
        Rule::V2PenaltyPre,
        Rule::V2PenaltyPost,
        Rule::GapPool,
        Rule::V1Snapshot,
        Rule::QcEpoch,
        Rule::ThreeChain,
        Rule::TcWalk,
    ];
    /// The rules a client request triggers. They are served inside the retention, not the reverse.
    pub const RPC: [Rule; 3] = [
        Rule::RpcMasternodes,
        Rule::RpcMissedRounds,
        Rule::RpcGetLogs,
    ];

    /// What the rule reads when it runs on block `n`: (class, blocks below `n`). Empty when the
    /// rule does not run on `n` (another fork row or consensus version is in force).
    pub fn lookback(self, spec: &ChainSpec, n: u64) -> Result<Vec<(Class, u64)>, Error> {
        let x = &spec.xdpos;
        let (e, g) = (x.epoch, x.gap);
        if e == 0 {
            return Err(Error::ZeroEpoch);
        }
        let switch = x.v2.as_ref().map(|v| v.switch_block);
        let v2 = switch.is_some_and(|s| n > s);
        let comeback = switch.is_some_and(|s| n > e.saturating_add(s));
        let w = u64::try_from(RANGE_RETURN_SIGNER).map_err(|_| Error::Overflow)?;
        let two = e.saturating_mul(2);
        let pool = e.saturating_mul(3).saturating_add(g) - 1; // see TieredReward
        let reads: Vec<(Class, u64)> = match self {
            // reward.go:80-91: blocks n-2r+1 ..= n-1 and the checkpoint header n-2r. Before
            // TIPSigning a sign transaction needs its receipt (reward.go:131-145).
            Self::V1Reward if reward::rule(spec, n) == Pay::V1 => {
                let r = checkpoint_period(spec).saturating_mul(2);
                let mut v = vec![(Class::Header, r), (Class::Body, r - 1)];
                if !spec
                    .effective
                    .tip_signing
                    .is_active(n.saturating_sub(r - 1))
                {
                    v.push((Class::Receipt, r - 1));
                }
                v
            }
            // engine_v2_hooks.go:681-728: walk to the second epoch switch below n (its header is
            // read), bodies of every block above it. Two epochs of at most E blocks.
            Self::V2Reward if reward::rule(spec, n) == Pay::V2 => {
                vec![(Class::Header, two), (Class::Body, two - 1)]
            }
            // The same window, plus GetStandbynodes(h) at engine_v2_hooks.go:985: the snapshot of
            // the gap block of h's epoch, h being the switch up to 2E below n and the gap block
            // up to E-1+G below h.
            Self::TieredReward if matches!(reward::rule(spec, n), Pay::Tiered { .. }) => {
                if pool_needed(x.v2.as_ref(), n) {
                    vec![
                        (Class::Header, pool),
                        (Class::Body, two - 1),
                        (Class::GapSnapshot, pool),
                    ]
                } else {
                    vec![(Class::Header, two), (Class::Body, two - 1)]
                }
            }
            // hooks.go:182-296: headers n-E+1 ..= n-1 (sealers) and n-E (masternodes), the header
            // n-(LimitPenaltyEpoch+1)*E (comeback `Penalties`, hooks.go:200), the last 150 blocks
            // (hooks.go:260), the signer snapshot of the gap block n-G.
            Self::V1Penalty if !v2 && spec.effective.tip_signing.is_active(n) => vec![
                (Class::Header, (LIMIT_PENALTY_EPOCH + 1).saturating_mul(e)),
                (Class::Body, w.min(e)),
                (Class::GapSnapshot, g),
            ],
            // engine_v2_hooks.go:346 (walk to the previous switch, one epoch), :390-433 (150 blocks).
            Self::V2PenaltyPre if v2 && !spec.tip_upgrade_penalty.is_active(n) => {
                let mut v = vec![(Class::Header, e)];
                if comeback {
                    v.push((Class::Body, w.min(e)));
                }
                v
            }
            // engine_v2_hooks.go:471-511: `Penalties` of the last `limit` switches (epochSwitch.go:34
            // walks one epoch per step) and the bodies of the epoch that ends. `limit` is the
            // config's `limitPenaltyEpoch` (1 when 0) of any round the block can have.
            Self::V2PenaltyPost if v2 && spec.tip_upgrade_penalty.is_active(n) => {
                let limit = post_limit(x.v2.as_ref(), n);
                let mut v = vec![(Class::Header, limit.saturating_mul(e))];
                if comeback {
                    v.push((Class::Body, e));
                }
                v
            }
            // engine.go:915-927, :2036: the snapshot of the gap block `n - n%E - G`.
            Self::GapPool if v2 => {
                vec![(Class::Header, e - 1 + g), (Class::GapSnapshot, e - 1 + g)]
            }
            // xdpos.go:2305-2345: walk back to the last gap block, at most E blocks.
            Self::V1Snapshot if !v2 => vec![(Class::Header, e), (Class::GapSnapshot, e)],
            // utils.go:322: the epoch switch of the block the QC names, at most E below n.
            Self::QcEpoch if v2 => vec![(Class::Header, e)],
            // utils.go:530-598: parent and grandparent of the block the QC names.
            Self::ThreeChain if switch.is_some_and(|s| n > s.saturating_add(2)) => {
                vec![(Class::Header, 3)]
            }
            // timeout.go:334-362: the switch of the TC's epoch; xOne serves the previous epoch,
            // so up to 2E below n, and its gap snapshot up to E-1+G below that switch.
            Self::TcWalk if v2 => vec![(Class::Header, two), (Class::GapSnapshot, pool)],
            // api_extended.go:171-215: switch of the block (E-1), then its gap block (E-1+G).
            Self::RpcMasternodes if v2 => {
                let l = two.saturating_add(g) - 2;
                vec![(Class::Header, l), (Class::GapSnapshot, l)]
            }
            // api_extended.go:328, utils.go:824: the switch of the block's epoch.
            Self::RpcMissedRounds if v2 => vec![(Class::Header, e)],
            // The receipts of the block itself, and of each block of the range.
            Self::RpcGetLogs => vec![(Class::Receipt, 0)],
            _ => Vec::new(),
        };
        Ok(reads)
    }
}

/// The configs a block can run under. Its round is at least `n - switch`, since every block of a
/// chain has a higher round than its parent, and has no upper bound.
fn configs_from(v2: &V2, n: u64) -> impl Iterator<Item = &V2Config> {
    let round = n.saturating_sub(v2.switch_block);
    let from = v2
        .configs
        .range(..=round)
        .next_back()
        .map_or(0, |(k, _)| *k);
    v2.configs.range(from..).map(|(_, c)| c)
}

fn post_limit(v2: Option<&V2>, n: u64) -> u64 {
    v2.and_then(|v| {
        configs_from(v, n)
            .map(|c| post_upgrade_limit(c.limit_penalty_epoch))
            .max()
    })
    .unwrap_or(1)
}

fn pool_needed(v2: Option<&V2>, n: u64) -> bool {
    v2.is_some_and(|v| {
        configs_from(v, n).any(|c| c.max_protector_nodes.saturating_add(c.max_observer_nodes) > 0)
    })
}

/// What a pivoted node keeps at and below the pivot: the largest window each class needs for any
/// rule that can run on the next `epoch` blocks, with the rule that sets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    pub pivot: u64,
    windows: BTreeMap<Class, (u64, Rule)>,
}

impl Retention {
    /// The window of every class for a pivot. A rule that runs on block `pivot + 1 + j` and reads
    /// `l` blocks below it needs `l - j` blocks at and below the pivot. The first block after
    /// the pivot can be a reward block, so `j = 0` is in the range. A switch comes within `epoch`
    /// blocks of any block, so `j` stops at `epoch - 1`; the fork rows inside that range are
    /// evaluated too, and the larger need wins.
    pub fn derive(spec: &ChainSpec, pivot: u64) -> Result<Self, Error> {
        if spec.xdpos.epoch == 0 {
            return Err(Error::ZeroEpoch);
        }
        let mut windows: BTreeMap<Class, (u64, Rule)> = BTreeMap::new();
        for j in 0..spec.xdpos.epoch {
            let n = pivot
                .checked_add(1)
                .and_then(|p| p.checked_add(j))
                .ok_or(Error::Overflow)?;
            for rule in Rule::CONSENSUS {
                for (class, l) in rule.lookback(spec, n)? {
                    let w = l.saturating_sub(j);
                    if w > windows.get(&class).map_or(0, |(have, _)| *have) {
                        windows.insert(class, (w, rule));
                    }
                }
            }
        }
        Ok(Self { pivot, windows })
    }

    /// A retention that is exactly one rule's lookback on block `n` (the pivot is `n - 1`).
    pub fn of_rule(spec: &ChainSpec, rule: Rule, n: u64) -> Result<Self, Error> {
        let mut windows = BTreeMap::new();
        for (class, l) in rule.lookback(spec, n)? {
            windows.insert(class, (l, rule));
        }
        Ok(Self {
            pivot: n.saturating_sub(1),
            windows,
        })
    }

    /// Blocks kept at and below the pivot for `class`, and the rule that sets it.
    pub fn window(&self, class: Class) -> (u64, Option<Rule>) {
        match self.windows.get(&class) {
            Some((w, r)) => (*w, Some(*r)),
            None => (0, None),
        }
    }

    /// The first block of `class` that is kept. Above the pivot everything is the node's own.
    pub fn floor(&self, class: Class) -> u64 {
        if class == Class::State {
            return self.pivot;
        }
        self.pivot
            .saturating_add(1)
            .saturating_sub(self.window(class).0)
    }

    /// `block` of `class` is held, or the typed refusal. There is no empty answer.
    pub fn is_within(&self, class: Class, block: u64) -> Result<(), Missing> {
        let floor = self.floor(class);
        if block >= floor {
            return Ok(());
        }
        Err(Missing::UnsupportedHistory {
            class,
            block,
            floor,
            pivot: self.pivot,
        })
    }

    /// A request of kind `rule` about `block` can be served: every class it reads is held down to
    /// `block - lookback`.
    pub fn serves(&self, spec: &ChainSpec, rule: Rule, block: u64) -> Result<(), Error> {
        for (class, l) in rule.lookback(spec, block)? {
            self.is_within(class, block.saturating_sub(l))?;
        }
        Ok(())
    }
}
