//! Who builds the next block (`spec/producer/schedule.md`).
//!
//! V1: the masternode after the parent's creator is in turn; any other waits ten seconds per hop
//! (twenty once the hops reach the next checkpoint). V2: the leader of a round is
//! `masternodes[round % epoch % len]`, and a round this node already signed a proposal for is never
//! proposed again.

use alloy_primitives::Address;
use xone_consensus::{
    unit::Sealer,
    v1::turn,
    v2::{masternodes, safety::Kind},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V1Slot {
    /// The next masternode after the parent's creator.
    InTurn,
    /// `wait` seconds after the parent's timestamp, this node may seal out of turn.
    OutOfTurn { wait: u64 },
    /// Not a masternode of the epoch, or the parent's creator is not one.
    No,
}

/// geth `YourTurn` before the V2 switch (`consensus/XDPoS/xdpos.go:1990-2033`).
pub fn v1_slot(
    masternodes: &[Address],
    parent_number: u64,
    parent_creator: Option<Address>,
    me: Address,
    epoch: u64,
) -> Result<V1Slot, xone_consensus::verify::HeaderError> {
    if turn::in_turn(masternodes, parent_number, parent_creator, me)? {
        return Ok(V1Slot::InTurn);
    }
    let pos = |a: Address| masternodes.iter().position(|m| *m == a);
    let (Some(cur), true) = (pos(me), parent_number != 0) else {
        return Ok(V1Slot::No);
    };
    let Some(pre) = parent_creator.and_then(pos) else {
        return Ok(V1Slot::No);
    };
    let to_i64 = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    let hops = turn::hop(to_i64(masternodes.len()), to_i64(pre), to_i64(cur));
    let wait = turn::out_of_turn_wait(u64::try_from(hops).unwrap_or(0), parent_number, epoch)?;
    Ok(V1Slot::OutOfTurn { wait })
}

/// May the slot seal now? (`xdpos.go:2022-2034`: `time.Now() - parent.Time >= wait`.)
pub fn v1_may_seal(slot: V1Slot, parent_time: u64, now: u64) -> bool {
    match slot {
        V1Slot::InTurn => true,
        V1Slot::OutOfTurn { wait } => now.saturating_sub(parent_time) >= wait,
        V1Slot::No => false,
    }
}

/// Why this node does not propose in a round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// geth `ErrAlreadyMined` (`engine_v2/mining.go:26-31`): the round is at or below the highest this
    /// node signed a proposal for.
    AlreadyMined { round: u64, highest: u64 },
    /// Not in the masternode list of the round.
    NotMasternode,
    /// Another masternode leads the round.
    NotLeader { leader: Address },
}

/// geth `yourturnAligned` (`engine_v2/mining.go:24-71`). `list` is the list of the round (the new list
/// at an epoch switch, else the parent's); the mined round comes from the safety unit, so a restart
/// does not forget it (xOneGo #624).
pub fn v2_turn<S: Sealer>(
    unit: &S,
    round: u64,
    epoch: u64,
    list: &[Address],
) -> Result<(), Skip> {
    let highest = unit.signed_up_to(Kind::Propose);
    if round <= highest {
        return Err(Skip::AlreadyMined { round, highest });
    }
    let me = unit.address();
    if !list.contains(&me) {
        return Err(Skip::NotMasternode);
    }
    match masternodes::leader(round, epoch, list) {
        Ok(l) if l == me => Ok(()),
        Ok(leader) => Err(Skip::NotLeader { leader }),
        Err(_) => Err(Skip::NotMasternode),
    }
}
