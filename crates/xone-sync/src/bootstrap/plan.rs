//! What a node must hold before the first block after a pivot can be verified. The plan is the
//! worst case: the first block after the pivot may open an epoch (a round can skip to the next
//! epoch at any block), so every rule is evaluated on `pivot + 1` and on each block up to one
//! epoch later, across every fork row and the V2 switch in that range, and the largest need wins
//! ([`Retention::derive`]). Nothing here reads a header: the plan is a function of the chain spec
//! and the pivot number alone.

use xone_chainspec::ChainSpec;

use super::anchor::Anchor;
use super::retention::{Class, Error, Retention, Rule};

/// A range of one class, inclusive, and the rule whose lookback sets its first block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Need {
    pub class: Class,
    pub first: u64,
    pub last: u64,
    pub rule: Option<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub pivot: u64,
    pub retention: Retention,
    /// Headers, bodies and receipts as one range each; one entry per gap snapshot (its gap
    /// block); the pivot state. The order is the class order.
    pub needs: Vec<Need>,
}

impl Plan {
    pub fn of(&self, class: Class) -> impl Iterator<Item = &Need> {
        self.needs.iter().filter(move |n| n.class == class)
    }
}

/// The plan for a pivot block.
pub fn plan_at(spec: &ChainSpec, pivot: u64) -> Result<Plan, Error> {
    let retention = Retention::derive(spec, pivot)?;
    let mut needs = Vec::new();
    for class in [Class::Header, Class::Body, Class::Receipt] {
        let (w, rule) = retention.window(class);
        if w > 0 {
            needs.push(Need {
                class,
                first: retention.floor(class),
                last: pivot,
                rule,
            });
        }
    }
    // The gap blocks are the numbers `k * epoch - gap` (`isGapNumber`, engine.go:924, xdpos.go:2320).
    let (e, g) = (spec.xdpos.epoch, spec.xdpos.gap);
    let (w, rule) = retention.window(Class::GapSnapshot);
    if w > 0 {
        let floor = retention.floor(Class::GapSnapshot);
        let mut k = floor.saturating_add(g).div_ceil(e).max(1);
        while let Some(at) = k.checked_mul(e).and_then(|m| m.checked_sub(g)) {
            if at > pivot {
                break;
            }
            if at > 0 {
                needs.push(Need {
                    class: Class::GapSnapshot,
                    first: at,
                    last: at,
                    rule,
                });
            }
            k += 1;
        }
    }
    needs.push(Need {
        class: Class::State,
        first: pivot,
        last: pivot,
        rule: None,
    });
    Ok(Plan {
        pivot,
        retention,
        needs,
    })
}

/// The plan for the pivot an anchor names.
pub fn pivot_plan(spec: &ChainSpec, anchor: &Anchor) -> Result<Plan, Error> {
    plan_at(spec, anchor.number)
}
