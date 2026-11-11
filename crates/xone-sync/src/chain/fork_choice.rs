//! XDC fork choice as a pure function. Spec: `spec/chain/fork-choice.md`.
//!
//! geth-XDC has no total-difficulty comparison at the pin. `writeBlockWithState` stores the
//! cumulative TD for the eth handshake (`core/blockchain.go:2262-2275`), and `writeBlockAndSetHead`
//! never reads it (`:2428-2464`). The choice is:
//!
//! 1. V2 (`number > V2.SwitchBlock`), parent is not the head, `number <= head number`: the block is
//!    stored as a side block (`keepsV2Head`, `:2414-2424`). Every V2 block has difficulty 1, so a
//!    same-height sibling never replaces the head; a higher block does.
//! 2. Otherwise the block becomes the head. If its parent is not the head, the chain is reorganised
//!    first (`:2438-2443`). In V1 this includes a sibling of the head and a block below the head.

/// What decides the choice about one executed block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    /// `number > V2.SwitchBlock`.
    pub v2: bool,
    pub parent_is_head: bool,
    pub number: u64,
    pub head_number: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The block extends the head.
    Extend,
    /// The block becomes the head on another branch (unwind, then apply the new branch).
    Reorg,
    /// The block is stored and the head stays.
    Side,
}

/// geth `writeBlockAndSetHead` after the state write (`core/blockchain.go:2433-2445`) and
/// `writeKnownBlock` (`:2236-2248`), which share the rule.
pub fn decide(f: Facts) -> Choice {
    if f.v2 && !f.parent_is_head && f.number <= f.head_number {
        Choice::Side
    } else if f.parent_is_head {
        Choice::Extend
    } else {
        Choice::Reorg
    }
}
