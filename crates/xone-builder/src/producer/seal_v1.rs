//! V1 `Prepare` and `Seal` (`spec/producer/seal-v1.md`).
//!
//! `prepare` is geth `Prepare` for a V1 block (`consensus/XDPoS/xdpos.go:1386-1496`): no coinbase and no
//! vote, the difficulty of the turn, a 32 byte vanity, at a checkpoint the masternodes of the next epoch
//! (and `Penalties` and `Validators`, the M2 assignment), a 65 byte seal space, mix digest zero, time
//! `parent + period` or now. `seal` is geth `Seal` (`xdpos.go:1727-1858`): the creator signs the seal hash
//! (M1, the last 65 bytes of `extraData`); past the first epoch the same signature is also the `validator`
//! when the creator is its own assigned M2. A block whose M2 is another masternode leaves `validator`
//! empty: that masternode co-signs ([`cosign`]) and the block is complete only then.

use alloy_primitives::{Address, Bytes, B256, U256};
use xone_consensus::{
    unit::{Sealer, UnitError},
    v1::{checkpoint::EXTRA_VANITY, double_validation::assigned_validator},
    v2::safety::{Intent, Kind},
    verify::HeaderError,
};
use xone_primitives::{Header, EXTRA_SEAL};

use super::build::EMPTY_UNCLE_HASH;

/// What a checkpoint header carries besides the vanity and the seal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checkpoint {
    /// The masternodes of the next epoch, in header order (`checkpoint_masternodes`).
    pub masternodes: Vec<Address>,
    /// `Penalties`: the penalties of this epoch (`xdpos.go:1439-1456`). Empty is no field.
    pub penalties: Vec<Address>,
    /// `Validators`: the M2 assignment, one 4 byte token per masternode (`HookValidator`).
    pub validators: Bytes,
}

pub struct Prepare<'a> {
    pub parent: &'a Header,
    pub gas_limit: u64,
    pub base_fee: Option<u64>,
    /// `len - Hop(len, pre, cur)` (`v1::turn::difficulty`).
    pub difficulty: i64,
    /// `timing::v1_block_time`.
    pub time: u64,
    /// `Some` when `number % epoch == 0 && number >= epoch`.
    pub checkpoint: Option<Checkpoint>,
    /// The miner's extra data (`--miner.extradata`): `Prepare` keeps its first 32 bytes and pads with zeros
    /// (`xdpos.go:1434-1437`).
    pub vanity: &'a [u8],
}

pub fn prepare(p: &Prepare<'_>) -> Header {
    let mut extra = p.vanity[..p.vanity.len().min(EXTRA_VANITY)].to_vec();
    extra.resize(EXTRA_VANITY, 0);
    let mut h = Header {
        parent_hash: p.parent.hash(),
        uncle_hash: EMPTY_UNCLE_HASH,
        number: p.parent.number + 1,
        gas_limit: p.gas_limit,
        difficulty: U256::from(u64::try_from(p.difficulty).unwrap_or(1)),
        timestamp: p.time,
        base_fee: p.base_fee.map(U256::from),
        ..Header::default()
    };
    if let Some(c) = &p.checkpoint {
        for m in &c.masternodes {
            extra.extend_from_slice(m.as_slice());
        }
        h.penalties = c.penalties.iter().flat_map(|a| a.0).collect::<Vec<u8>>().into();
        h.validators = c.validators.clone();
    }
    extra.extend_from_slice(&[0; EXTRA_SEAL]);
    h.extra_data = extra.into();
    h
}

/// A sealed header, and the masternode that still has to co-sign it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub header: Header,
    pub needs_m2: Option<Address>,
}

/// M1 (and M2 when it is this node). The height goes to the unit as `Kind::SealV1`: the same height with
/// another digest is refused, which also keeps a node from sealing two blocks of one height.
pub fn seal<S: Sealer>(
    mut h: Header,
    unit: &mut S,
    epoch: u64,
    checkpoint: Option<&Header>,
    tip_randomize: bool,
) -> Result<Sealed, SealError> {
    let me = unit.address();
    let sig = unit.sign(Intent::new(Kind::SealV1, h.number, h.seal_hash()))?;
    let at = h
        .extra_data
        .len()
        .checked_sub(EXTRA_SEAL)
        .ok_or(SealError::NoSealSpace)?;
    let mut extra = h.extra_data.to_vec();
    extra[at..].copy_from_slice(&sig.bytes);
    h.extra_data = extra.into();
    let mut needs_m2 = None;
    if h.number > epoch {
        let m2 = assigned_validator(&h, me, checkpoint, epoch, tip_randomize)?;
        if m2 == me {
            h.validator = sig.bytes;
        } else {
            needs_m2 = Some(m2);
        }
    }
    Ok(Sealed { header: h, needs_m2 })
}

/// The M2 co-signature of a block another masternode sealed, when this node is its assigned M2 (geth
/// `appendM2HeaderHook`). The digest is the seal hash, which leaves `validator` out.
pub fn cosign<S: Sealer>(
    mut h: Header,
    creator: Address,
    unit: &mut S,
    epoch: u64,
    checkpoint: Option<&Header>,
    tip_randomize: bool,
) -> Result<Option<Header>, SealError> {
    let me = unit.address();
    if !h.validator.is_empty()
        || assigned_validator(&h, creator, checkpoint, epoch, tip_randomize)? != me
    {
        return Ok(None);
    }
    let digest: B256 = h.seal_hash();
    h.validator = unit.cosign_v1(h.number, digest)?.bytes;
    Ok(Some(h))
}

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("the header has no room for the 65 byte seal")]
    NoSealSpace,
    #[error("{0}")]
    Unit(#[from] UnitError),
    #[error("{0}")]
    M2(#[from] HeaderError),
}
