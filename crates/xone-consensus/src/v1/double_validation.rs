//! V1 double validation: every block past the first epoch is sealed by its creator (M1, the
//! signature in the last 65 bytes of `extraData`) and co-signed by the validator (M2, the
//! `validator` field) that the checkpoint of the epoch assigns to that creator. Both sign the same
//! digest, `SealHash`, which leaves `validator` out of the hash (geth `HashNoValidator`).

use std::collections::BTreeMap;

use alloy_primitives::Address;
use xone_primitives::{ecrecover, Header};

use super::checkpoint::masternodes_of;
use crate::verify::HeaderError;

/// geth `EpocBlockRandomize` (`consensus/XDPoS/constants.go:38`): the M2 list lives in checkpoint
/// headers, which are multiples of 900.
pub const EPOCH_BLOCK_RANDOMIZE: u64 = 900;
/// geth `M2ByteLength` (`consensus/XDPoS/utils/constants.go:29`).
const M2_BYTE_LENGTH: usize = 4;

/// geth `ExtractValidatorsFromBytes` (`xdpos.go:2732-2745`): the `validators` field of a V1
/// checkpoint is one 4-byte ASCII decimal token per masternode, NUL padding trimmed from both
/// ends. One bad token empties the whole list, as in geth; [`m1_m2`] then fails on its length.
pub fn m2_tokens(bytes: &[u8]) -> Vec<i64> {
    let mut out = vec![];
    for chunk in bytes.chunks_exact(M2_BYTE_LENGTH) {
        let text = match (
            chunk.iter().position(|b| *b != 0),
            chunk.iter().rposition(|b| *b != 0),
        ) {
            (Some(s), Some(e)) => &chunk[s..=e],
            _ => &[][..],
        };
        match std::str::from_utf8(text)
            .ok()
            .and_then(|t| t.parse::<i64>().ok())
        {
            Some(v) => out.push(v),
            None => return vec![],
        }
    }
    out
}

/// geth `getM1M2` (`consensus/XDPoS/double_validation.go:162-180`): the M2 of each masternode. With
/// `m = len(masternodes)`, `moveM2` is `((number % epoch) / m) % m` once `TIPRandomize` is active
/// and 0 before, and masternode `i` gets `masternodes[(tokens[i] % m + moveM2) % m]`. A later
/// duplicate masternode overwrites an earlier one, as in geth's map. Fewer tokens than masternodes
/// is [`HeaderError::M2Assignment`].
pub fn m1_m2(
    masternodes: &[Address],
    tokens: &[i64],
    number: u64,
    epoch: u64,
    tip_randomize: bool,
) -> Result<BTreeMap<Address, Address>, HeaderError> {
    if tokens.len() < masternodes.len() {
        return Err(HeaderError::M2Assignment);
    }
    let mut out = BTreeMap::new();
    let (Ok(m), Ok(m_signed)) = (
        u64::try_from(masternodes.len()),
        i64::try_from(masternodes.len()),
    ) else {
        return Err(HeaderError::M2Assignment);
    };
    if m == 0 {
        return Ok(out);
    }
    let move_m2 = if tip_randomize {
        ((number % epoch.max(1)) / m) % m
    } else {
        0
    };
    for (m1, token) in masternodes.iter().zip(tokens) {
        // Go: `uint64(validators[i] % int64(maxMNs))`; a negative remainder wraps.
        let wrapped = u64::from_ne_bytes((token % m_signed).to_ne_bytes());
        let index = usize::try_from(wrapped.wrapping_add(move_m2) % m)
            .map_err(|_| HeaderError::M2Assignment)?;
        out.insert(*m1, masternodes[index]);
    }
    Ok(out)
}

/// The M2 assigned to `creator` for the block `h` (geth `GetValidator` and
/// `getValidatorWithParents`, `consensus/XDPoS/xdpos_api.go:85-108`, `:126-193`). `checkpoint` is
/// the checkpoint header of the block's epoch; when `h` is itself the checkpoint it is not read.
/// Blocks of the first epoch have no M2 (the zero address). A creator that is not among the
/// checkpoint's masternodes gets the zero address, as geth's map lookup does.
pub fn assigned_validator(
    h: &Header,
    creator: Address,
    checkpoint: Option<&Header>,
    epoch: u64,
    tip_randomize: bool,
) -> Result<Address, HeaderError> {
    let cp_no = super::checkpoint_number(h.number, epoch)?;
    if cp_no == 0 {
        return Ok(Address::ZERO);
    }
    let cp = if h.number == cp_no {
        h
    } else {
        checkpoint.ok_or(HeaderError::MissingCheckpointHeader(cp_no))?
    };
    if cp.number != cp_no || cp.number % EPOCH_BLOCK_RANDOMIZE != 0 {
        return Err(HeaderError::M2Assignment);
    }
    // geth `decodeMasternodesFromHeaderExtra` reads a too short `extraData` as an empty list;
    // here it is a typed error.
    let masternodes = masternodes_of(&cp.extra_data)?;
    let m1_m2 = m1_m2(
        &masternodes,
        &m2_tokens(&cp.validators),
        h.number,
        epoch,
        tip_randomize,
    )?;
    Ok(m1_m2.get(&creator).copied().unwrap_or(Address::ZERO))
}

/// geth `verifySeal`'s double validation (`xdpos.go:1347-1363`) for a block past the first epoch
/// under full verification: `validator` is not empty, recovers over the seal hash, and is the
/// `assigned()` M2 of the creator, which is only computed once the signature recovers (geth's
/// order). Returns the recovered validator.
pub fn check_double_validation(
    h: &Header,
    creator: Address,
    assigned: impl FnOnce() -> Result<Address, HeaderError>,
) -> Result<Address, HeaderError> {
    if h.validator.is_empty() {
        return Err(HeaderError::NoValidatorSignature);
    }
    let validator = ecrecover(&h.seal_hash(), &h.validator)
        .map_err(|_| HeaderError::InvalidValidatorSignature)?;
    if validator != assigned()? {
        return Err(HeaderError::FailedDoubleValidation { creator, validator });
    }
    Ok(validator)
}
