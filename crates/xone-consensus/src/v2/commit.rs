//! The 3-chain commit rule (geth `commitBlocks`, `engine_v2/utils.go:530-598`, called from
//! `processQC`, `utils.go:476`). When a QC certifies block `B` of round `r`, the parent of `B` has
//! round `r-1` and the grandparent has round `r-2`, the grandparent is committed. Pure: the state is
//! the highest committed block, passed in and returned.

use alloy_primitives::B256;

use super::Error;

/// A block as the rule sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    pub number: u64,
    pub round: u64,
    pub hash: B256,
    pub parent: B256,
}

pub trait Headers {
    fn link(&self, hash: B256) -> Option<Link>;
}

fn get<H: Headers>(h: &H, hash: B256, what: &'static str, number: u64) -> Result<Link, Error> {
    h.link(hash).ok_or(Error::Missing { what, number })
}

/// The block `proposed` (the block a QC certifies) commits, if any. `highest` is the highest block
/// committed so far; a candidate at or below it by round or by number is not committed again.
pub fn commit_blocks<H: Headers>(
    headers: &H,
    switch_block: u64,
    highest: Option<&Link>,
    proposed: &Link,
) -> Result<Option<Link>, Error> {
    // `proposedBlockHeader.Number.Int64()-2 <= SwitchBlock` (utils.go:534)
    if proposed.number.saturating_sub(2) <= switch_block {
        return Ok(None);
    }
    let parent = get(
        headers,
        proposed.parent,
        "parent",
        proposed.number.saturating_sub(1),
    )?;
    if proposed.round.wrapping_sub(1) != parent.round {
        return Ok(None);
    }
    let grand = get(
        headers,
        parent.parent,
        "grandparent",
        parent.number.saturating_sub(1),
    )?;
    if proposed.round.wrapping_sub(2) != grand.round {
        return Ok(None);
    }
    if highest.is_some_and(|h| h.round >= grand.round || h.number >= grand.number) {
        return Ok(None);
    }
    Ok(Some(grand))
}
