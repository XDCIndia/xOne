//! V2 `Prepare` and `Seal` (`spec/producer/seal-v2.md`).
//!
//! `prepare` is geth `Prepare` of the V2 engine (`consensus/XDPoS/engines/engine_v2/engine.go:651-751`): the
//! parent must be the block of the highest QC (else `ErrNotReadyToPropose`), `extraData` is the version byte
//! and `ExtraFields_v2{round, highest QC}`, difficulty 1, the coinbase is the signer, at an epoch switch the
//! new `Validators` and `Penalties`, time `parent + minePeriod(round)` or now. `seal` is `Seal`
//! (`engine.go:812-845`): the signature over the V2 signature hash goes in `validator`, and the round is
//! marked mined. Here the mark is the unit's `Propose` slot, written before the signature exists.

use alloy_primitives::{Address, U256};
use xone_consensus::{
    unit::{Sealer, UnitError},
    v2::safety::{Intent, Kind},
};
use xone_primitives::{ExtraFieldsV2, Header, QuorumCert};

use super::build::EMPTY_UNCLE_HASH;

pub struct Prepare<'a> {
    pub parent: &'a Header,
    pub round: u64,
    /// The highest QC of the node (`highestQuorumCert`).
    pub qc: &'a QuorumCert,
    pub me: Address,
    pub gas_limit: u64,
    pub base_fee: Option<u64>,
    /// `timing::v2_block_time` with the mine period of the round's config row.
    pub time: u64,
    /// At an epoch switch: the masternodes of the new epoch (in order) and the penalties.
    pub switch: Option<(Vec<Address>, Vec<Address>)>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// geth `ErrNotReadyToPropose` (`engine.go:690-697`).
    #[error("the parent {parent} is not the block of the highest QC {qc}")]
    NotReady {
        parent: alloy_primitives::B256,
        qc: alloy_primitives::B256,
    },
    #[error("{0}")]
    Unit(String),
}

impl From<UnitError> for Error {
    fn from(e: UnitError) -> Self {
        Self::Unit(e.to_string())
    }
}

pub fn prepare(p: &Prepare<'_>) -> Result<Header, Error> {
    let parent = p.parent.hash();
    if parent != p.qc.proposed_block_info.hash {
        return Err(Error::NotReady { parent, qc: p.qc.proposed_block_info.hash });
    }
    let mut h = Header {
        parent_hash: parent,
        uncle_hash: EMPTY_UNCLE_HASH,
        coinbase: p.me,
        number: p.parent.number + 1,
        gas_limit: p.gas_limit,
        difficulty: U256::from(1),
        timestamp: p.time,
        base_fee: p.base_fee.map(U256::from),
        extra_data: ExtraFieldsV2 { round: p.round, quorum_cert: p.qc.clone() }
            .encode_extra()
            .into(),
        ..Header::default()
    };
    if let Some((masternodes, penalties)) = &p.switch {
        h.validators = masternodes.iter().flat_map(|a| a.0).collect::<Vec<u8>>().into();
        h.penalties = penalties.iter().flat_map(|a| a.0).collect::<Vec<u8>>().into();
    }
    Ok(h)
}

/// The proposal: persisted as `Intent::Propose` for the header's round, then signed.
pub fn seal<S: Sealer>(mut h: Header, round: u64, unit: &mut S) -> Result<Header, Error> {
    let sig = unit.sign(Intent::new(Kind::Propose, round, h.v2_sig_hash()))?;
    h.validator = sig.bytes;
    Ok(h)
}
