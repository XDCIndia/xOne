//! Epoch switches of V2: detection by round, and the lists a switch header must carry.

use alloy_primitives::{Address, B256};
use xone_chainspec::{ChainSpec, V2};
use xone_primitives::{ExtraFieldsV2, Header};

use super::{
    gap_snapshot::SnapshotV2,
    masternodes::{calc, first_v2_block, max_masternodes},
    Error,
};
use crate::chain::{BlockReader, PenaltyHistory};
use crate::penalty;

/// geth `IsEpochSwitch` (`engine_v2/engine.go:1952-2013`): `(is switch, epoch number)`. The V2 switch
/// block is a switch, a V1 block is not, the first V2 block (its QC is for the switch block) is one,
/// and any other block is one when the round of its QC is before the start round of its own epoch.
/// Epochs are counted in rounds, so a block number says nothing here.
pub fn is_epoch_switch(
    v2: &V2,
    epoch: u64,
    number: u64,
    fields: Option<&ExtraFieldsV2>,
) -> Result<(bool, u64), Error> {
    let epoch_of = |round: u64| round.checked_div(epoch).ok_or(Error::ZeroEpoch);
    if number == v2.switch_block {
        return Ok((true, epoch_of(number)?));
    }
    if number < v2.switch_block {
        return Ok((false, 0));
    }
    let fields = fields.ok_or(Error::Missing {
        what: "V2 extra fields",
        number,
    })?;
    let round = fields.round;
    let num = v2.switch_epoch.wrapping_add(epoch_of(round)?);
    let qc = &fields.quorum_cert.proposed_block_info;
    if qc.number == v2.switch_block {
        return Ok((true, num));
    }
    let start = round - round.checked_rem(epoch).ok_or(Error::ZeroEpoch)?;
    Ok((qc.round < start, num))
}

/// [`is_epoch_switch`] of a header.
pub fn of_header(v2: &V2, epoch: u64, h: &Header) -> Result<(bool, u64), Error> {
    if h.number <= v2.switch_block {
        return is_epoch_switch(v2, epoch, h.number, None);
    }
    let fields = h.extra_fields_v2().map_err(|_| Error::Missing {
        what: "V2 extra fields",
        number: h.number,
    })?;
    is_epoch_switch(v2, epoch, h.number, Some(&fields))
}

/// geth `isEpochSwitchAtRound` (`engine_v2/timeout.go:505-526`): whether a block of `round` on this
/// parent opens an epoch. Used by the producer to pick the list before the block exists.
pub fn is_epoch_switch_at_round(
    v2: &V2,
    epoch: u64,
    round: u64,
    parent_number: u64,
    parent_round: u64,
) -> Result<(bool, u64), Error> {
    let num = v2
        .switch_epoch
        .wrapping_add(round.checked_div(epoch).ok_or(Error::ZeroEpoch)?);
    if parent_number == v2.switch_block {
        return Ok((true, num));
    }
    if round <= parent_round {
        return Ok((false, num));
    }
    let start = round - round.checked_rem(epoch).ok_or(Error::ZeroEpoch)?;
    Ok((parent_round < start, num))
}

/// What the switch block being verified needs to know.
#[derive(Debug, Clone)]
pub struct SwitchQuery {
    pub number: u64,
    pub parent: B256,
    /// Round of the block itself: it selects `maxMasternodes`.
    pub round: u64,
    /// Round of its parent: it selects the config of the penalty rules.
    pub parent_round: u64,
    /// Masternodes of the epoch that ends.
    pub parent_masternodes: Vec<Address>,
}

/// The `Validators` (in order) and `Penalties` of a switch header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub validators: Vec<Address>,
    pub penalties: Vec<Address>,
}

/// geth `calcMasternodes` (`engine.go:2023-2096`) with the penalties from `penalties`, which gets the
/// pool. The block after the V2 switch block has no penalties and its pool is only cut; a switch with
/// an empty pool is an error, and one whose pool is all penalised yields an empty list that the
/// header check then refuses.
pub fn lists(
    spec: &ChainSpec,
    q: &SwitchQuery,
    snapshot: &SnapshotV2,
    penalties: impl FnOnce(&[Address]) -> Result<Vec<Address>, Error>,
) -> Result<Expected, Error> {
    let v2 = spec.xdpos.v2.as_ref().ok_or(Error::NoV2)?;
    let max = max_masternodes(v2, q.round)?;
    let pool = &snapshot.candidates;
    if pool.is_empty() {
        return Err(Error::SnapshotEmpty);
    }
    if q.number == v2.switch_block + 1 {
        return Ok(Expected {
            validators: first_v2_block(pool, max),
            penalties: Vec::new(),
        });
    }
    let penalties = penalties(pool)?;
    Ok(Expected {
        validators: calc(pool, &penalties, max),
        penalties,
    })
}

/// [`lists`] with the penalties of the V2 penalty engine (XB-10) over the chain reader.
pub fn expected<R: BlockReader + PenaltyHistory>(
    spec: &ChainSpec,
    reader: &R,
    q: &SwitchQuery,
    snapshot: &SnapshotV2,
) -> Result<Expected, Error> {
    lists(spec, q, snapshot, |pool| {
        Ok(penalty::v2::penalties(
            spec,
            reader,
            &penalty::v2::Input {
                number: q.number,
                parent: q.parent,
                round: q.parent_round,
                parent_masternodes: &q.parent_masternodes,
                candidates: pool,
            },
        )?)
    })
}
