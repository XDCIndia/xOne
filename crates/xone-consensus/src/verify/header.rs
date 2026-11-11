//! Header verification as geth-XDC runs it on every accepting node, in geth's
//! order, with the strict (full verification) branch everywhere geth has a
//! bulk-sync tolerance. Pure: the caller supplies the parent, the clock and the
//! masternode data of the context; nothing is read from the environment.

use std::collections::BTreeSet;

use alloy_primitives::{b256, keccak256, Address, B256, U256};
use alloy_rlp::RlpEncodable;
use k256::ecdsa::Signature as EcdsaSignature;
use xone_chainspec::{ChainSpec, V2};
use xone_primitives::{ecrecover, BlockInfo, Header, QuorumCert, EXTRA_SEAL};

use super::error::HeaderError;

const EXTRA_VANITY: usize = 32;
/// geth `EpocBlockRandomize` (`consensus/XDPoS/constants.go:38`).
const EPOCH_BLOCK_RANDOMIZE: u64 = 900;
/// geth `params.XDCBaseFee`, 12.5 gwei (`params/protocol_params.go:175`).
const XDC_BASE_FEE: u64 = 12_500_000_000;
/// geth `gas2500xMultiplier` (`params/xdc_gas.go:32`).
const GAS_2500X_MULTIPLIER: u64 = 50;
/// geth `params.MinGasLimit` and `GasLimitBoundDivisor` (`params/protocol_params.go:26-27`).
const MIN_GAS_LIMIT: u64 = 5000;
const GAS_LIMIT_BOUND_DIVISOR: u64 = 1024;
/// Chain id of Apothem, where geth turns the V1 full verification off
/// (`consensus/XDPoS/xdpos.go:1210`, `:1371`).
const APOTHEM_CHAIN_ID: u64 = 51;
/// geth `types.EmptyRootHash` and `EmptyWithdrawalsHash` (`core/types/hashes.go:26`, `:41`).
const EMPTY_ROOT: B256 = b256!("56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421");
/// geth `types.EmptyRequestsHash` (`core/types/hashes.go:44`).
const EMPTY_REQUESTS_HASH: B256 =
    b256!("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");

/// What the caller knows about a V1 block besides its parent.
#[derive(Debug, Clone, Default)]
pub struct V1Context {
    /// Masternodes of the block's epoch, in checkpoint order (geth `GetMasternodes(header)`).
    pub masternodes: Vec<Address>,
    /// Masternodes of the parent's epoch (geth `GetMasternodes(parent)`), the list of `YourTurn`.
    pub parent_masternodes: Vec<Address>,
    /// Signers of the snapshot at the parent (geth `snap.Signers`).
    pub signers: Vec<Address>,
    /// The checkpoint header of the block's epoch, for the M2 assignment. Unused when the
    /// block is the checkpoint (geth then reads the header itself).
    pub checkpoint: Option<Header>,
    /// The signer list a checkpoint block must carry (geth `checkSignersOnCheckpoint`).
    pub checkpoint_signers: Vec<Address>,
}

/// What the caller knows about a V2 block besides its parent.
#[derive(Debug, Clone, Default)]
pub struct V2Context {
    /// Masternodes of the block's epoch in epoch-switch order. At an epoch switch this is the
    /// list the `Validators` field must equal.
    pub masternodes: Vec<Address>,
    /// Penalties of the block's epoch. At an epoch switch this is the list `Penalties` must equal.
    pub penalties: Vec<Address>,
    /// Masternodes of the parent's epoch, which sign the quorum certificate. `None` when the
    /// parent is the V1 switch block: the list is then in the parent's `extra_data`.
    pub parent_masternodes: Option<Vec<Address>>,
    /// Number of the epoch-switch block of the parent's epoch.
    pub parent_epoch_switch: u64,
}

#[derive(Debug, Clone)]
pub enum Context {
    V1(Box<V1Context>),
    V2(V2Context),
}

pub struct HeaderVerifier<'a> {
    spec: &'a ChainSpec,
}

fn addresses(bytes: &[u8]) -> Vec<Address> {
    // geth `contracts.ExtractAddressFromBytes`: a trailing partial address is dropped.
    bytes
        .chunks_exact(Address::len_bytes())
        .map(Address::from_slice)
        .collect()
}

/// geth `utils.CompareSignersLists`: equal as multisets.
fn same_set(a: &[Address], b: &[Address]) -> bool {
    let (mut a, mut b) = (a.to_vec(), b.to_vec());
    a.sort();
    b.sort();
    a == b
}

fn position(list: &[Address], x: Address) -> i64 {
    list.iter().position(|a| *a == x).map_or(-1, |i| i as i64)
}

/// geth `Hop` (`consensus/XDPoS/utils/utils.go:25`).
fn hop(len: i64, pre: i64, cur: i64) -> i64 {
    match pre.cmp(&cur) {
        std::cmp::Ordering::Less => cur - (pre + 1),
        std::cmp::Ordering::Greater => (len - pre) + (cur - 1),
        std::cmp::Ordering::Equal => len - 1,
    }
}

fn nonce_ok(h: &Header) -> bool {
    h.nonce.0 == [0u8; 8] || h.nonce.0 == [0xffu8; 8]
}

/// geth `VerifyGaslimit` (`consensus/misc/gaslimit.go:26`).
fn verify_gas_limit(parent: u64, have: u64) -> Result<(), HeaderError> {
    let diff = (parent as i64).wrapping_sub(have as i64).unsigned_abs();
    if diff >= parent / GAS_LIMIT_BOUND_DIVISOR {
        return Err(HeaderError::InvalidGasLimit { have, parent });
    }
    if have < MIN_GAS_LIMIT {
        return Err(HeaderError::GasLimitTooLow(have));
    }
    Ok(())
}

/// geth `types.VoteForSign` (`core/types/consensus_v2.go`): what a QC signature signs.
#[derive(RlpEncodable)]
struct VoteForSign<'a> {
    proposed_block_info: &'a BlockInfo,
    gap_number: u64,
}

impl<'a> HeaderVerifier<'a> {
    pub fn new(spec: &'a ChainSpec) -> Self {
        Self { spec }
    }

    fn v2(&self) -> Option<&V2> {
        self.spec.xdpos.v2.as_ref()
    }

    /// True if the XDPoS V2 rules apply to `number` (geth `IsV2Block`: above the switch block).
    pub fn is_v2(&self, number: u64) -> bool {
        self.v2().is_some_and(|v2| number > v2.switch_block)
    }

    /// Verify `header` against its `parent`. `now` is the caller's clock in seconds.
    pub fn verify(
        &self,
        header: &Header,
        parent: &Header,
        now: u64,
        ctx: &Context,
    ) -> Result<(), HeaderError> {
        if header.number == 0 {
            return Err(HeaderError::Genesis);
        }
        self.unsealed_fields(header)?;
        match (self.is_v2(header.number), ctx) {
            (false, Context::V1(c)) => self.verify_v1(header, parent, now, c),
            (true, Context::V2(c)) => self.verify_v2(header, parent, now, c),
            _ => Err(HeaderError::WrongContext(header.number)),
        }
    }

    /// geth `verifyNoUnsealedFields` (`consensus/XDPoS/xdpos.go:766`).
    fn unsealed_fields(&self, h: &Header) -> Result<(), HeaderError> {
        let ok = if self.spec.amsterdam.is_active(h.number) {
            h.slot_number.is_none()
                && h.block_access_list_hash.is_some()
                && h.withdrawals_hash == Some(EMPTY_ROOT)
                && h.blob_gas_used == Some(0)
                && h.excess_blob_gas == Some(0)
                && h.parent_beacon_root == Some(B256::ZERO)
                && h.requests_hash == Some(EMPTY_REQUESTS_HASH)
        } else {
            h.withdrawals_hash.is_none()
                && h.blob_gas_used.is_none()
                && h.excess_blob_gas.is_none()
                && h.parent_beacon_root.is_none()
                && h.requests_hash.is_none()
                && h.block_access_list_hash.is_none()
                && h.slot_number.is_none()
        };
        ok.then_some(()).ok_or(HeaderError::UnsealedFields)
    }

    fn parent_link(&self, h: &Header, parent: &Header) -> Result<(), HeaderError> {
        if parent.number != h.number - 1 || parent.hash() != h.parent_hash {
            return Err(HeaderError::UnknownAncestor(h.number - 1));
        }
        Ok(())
    }

    // ---------------------------------------------------------------- V1

    /// geth `XDPoS.verifyHeader` V1 branch, `verifyCascadingFields`, `verifySeal`
    /// (`consensus/XDPoS/xdpos.go:808-1375`).
    fn verify_v1(
        &self,
        h: &Header,
        parent: &Header,
        now: u64,
        ctx: &V1Context,
    ) -> Result<(), HeaderError> {
        let x = &self.spec.xdpos;
        let (n, epoch) = (h.number, x.epoch);
        if h.timestamp > now {
            return Err(HeaderError::FutureBlock {
                time: h.timestamp,
                now,
            });
        }
        let checkpoint = n % epoch == 0;
        if checkpoint && !h.coinbase.is_zero() {
            return Err(HeaderError::CheckpointBeneficiary);
        }
        if !nonce_ok(h) {
            return Err(HeaderError::InvalidVote);
        }
        if checkpoint && h.nonce.0 != [0u8; 8] {
            return Err(HeaderError::InvalidCheckpointVote);
        }
        let len = h.extra_data.len();
        if len < EXTRA_VANITY {
            return Err(HeaderError::MissingVanity);
        }
        if len < EXTRA_VANITY + EXTRA_SEAL {
            return Err(HeaderError::MissingSignature);
        }
        let signer_bytes = len - EXTRA_VANITY - EXTRA_SEAL;
        let at_or_past_switch = self.v2().is_some_and(|v2| n >= v2.switch_block);
        if !checkpoint && signer_bytes != 0 && !at_or_past_switch {
            return Err(HeaderError::ExtraSigners);
        }
        if checkpoint && !signer_bytes.is_multiple_of(Address::len_bytes()) {
            return Err(HeaderError::InvalidCheckpointSigners);
        }
        if !h.mix_digest.is_zero() {
            return Err(HeaderError::InvalidMixDigest);
        }
        if h.uncle_hash != keccak256([alloy_rlp::EMPTY_LIST_CODE]) {
            return Err(HeaderError::InvalidUncleHash);
        }
        // Cascading fields.
        self.parent_link(h, parent)?;
        if parent.timestamp.wrapping_add(x.period) > h.timestamp {
            return Err(HeaderError::InvalidTimestamp {
                time: h.timestamp,
                parent: parent.timestamp,
                period: x.period,
            });
        }
        let full_verify = self.spec.chain_id != APOTHEM_CHAIN_ID;
        if checkpoint && full_verify {
            let listed = addresses(&h.extra_data[EXTRA_VANITY..len - EXTRA_SEAL]);
            if !same_set(&listed, &ctx.checkpoint_signers) {
                return Err(HeaderError::InvalidCheckpointSigners);
            }
        }
        self.verify_seal_v1(h, parent, full_verify, ctx)
    }

    fn verify_seal_v1(
        &self,
        h: &Header,
        parent: &Header,
        double_validation: bool,
        ctx: &V1Context,
    ) -> Result<(), HeaderError> {
        let (n, epoch) = (h.number, self.spec.xdpos.epoch);
        let creator = h.v1_signer()?;

        // The parent's creator is geth `whoIsCreator(parent)`, which is also the
        // newest entry of the snapshot's recent signers.
        let parent_creator = (parent.number != 0)
            .then(|| parent.v1_signer().ok())
            .flatten();
        let want = self.v1_difficulty(parent, parent_creator, creator, &ctx.parent_masternodes);
        let have = h.difficulty.as_limbs()[0] as i64;
        if want != 0 && have != want {
            return Err(HeaderError::InvalidDifficulty { have, want });
        }

        if !ctx.signers.contains(&creator) && !ctx.masternodes.contains(&creator) {
            return Err(HeaderError::Unauthorized(creator));
        }
        if ctx.masternodes.len() > 1 && n % epoch != 0 && parent_creator == Some(creator) {
            return Err(HeaderError::Unauthorized(creator));
        }

        if double_validation && n > epoch {
            if h.validator.is_empty() {
                return Err(HeaderError::NoValidatorSignature);
            }
            let validator = ecrecover(&h.seal_hash(), &h.validator)
                .map_err(|_| HeaderError::InvalidValidatorSignature)?;
            let assigned = self.v1_assigned_validator(h, creator, ctx)?;
            if validator != assigned {
                return Err(HeaderError::FailedDoubleValidation { creator, validator });
            }
        }
        Ok(())
    }

    /// geth `calcDifficulty` over `YourTurn` (`xdpos.go:1875`, `:1983`). `0` stands for the
    /// cases where `YourTurn` fails and geth skips the comparison.
    fn v1_difficulty(
        &self,
        parent: &Header,
        parent_creator: Option<Address>,
        creator: Address,
        masternodes: &[Address],
    ) -> i64 {
        let len = masternodes.len() as i64;
        if len == 0 {
            return 0;
        }
        let pre = match (parent.number, parent_creator) {
            (0, _) => -1,
            (_, Some(p)) => position(masternodes, p),
            (_, None) => return 0,
        };
        len - hop(len, pre, position(masternodes, creator))
    }

    /// geth `getValidatorWithParents` and `getM1M2` (`xdpos_api.go:126`, `double_validation.go:57`).
    fn v1_assigned_validator(
        &self,
        h: &Header,
        creator: Address,
        ctx: &V1Context,
    ) -> Result<Address, HeaderError> {
        let epoch = self.spec.xdpos.epoch;
        let cp_no = h.number - h.number % epoch;
        if cp_no == 0 {
            return Ok(Address::ZERO);
        }
        let cp = if h.number.is_multiple_of(epoch) {
            h
        } else {
            ctx.checkpoint.as_ref().ok_or(HeaderError::M2Assignment)?
        };
        if cp.number != cp_no || cp.number % EPOCH_BLOCK_RANDOMIZE != 0 {
            return Err(HeaderError::M2Assignment);
        }
        let masternodes = match cp.extra_data.len().checked_sub(EXTRA_VANITY + EXTRA_SEAL) {
            Some(_) => addresses(&cp.extra_data[EXTRA_VANITY..cp.extra_data.len() - EXTRA_SEAL]),
            None => vec![],
        };
        let max = masternodes.len() as i64;
        let tokens = m2_tokens(&cp.validators);
        if (tokens.len() as i64) < max {
            return Err(HeaderError::M2Assignment);
        }
        if max == 0 {
            return Ok(Address::ZERO);
        }
        let move_m2 = if self.spec.effective.tip_randomize.is_active(h.number) {
            ((h.number % epoch) / max as u64) % max as u64
        } else {
            0
        };
        Ok(masternodes
            .iter()
            .position(|m| *m == creator)
            .map_or(Address::ZERO, |i| {
                let index = ((tokens[i] % max) as u64 + move_m2) % max as u64;
                masternodes[index as usize]
            }))
    }

    // ---------------------------------------------------------------- V2

    /// geth `engine_v2.verifyHeader` under full verification
    /// (`consensus/XDPoS/engines/engine_v2/verifyHeader.go:68`).
    fn verify_v2(
        &self,
        h: &Header,
        parent: &Header,
        now: u64,
        ctx: &V2Context,
    ) -> Result<(), HeaderError> {
        let v2 = self.v2().ok_or(HeaderError::NoV2Section)?;
        let epoch = self.spec.xdpos.epoch;
        match h.validator.len() {
            0 => return Err(HeaderError::NoValidatorSignatureV2),
            65 => {}
            n => return Err(HeaderError::ValidatorSignatureLength(n)),
        }
        if h.timestamp > now {
            return Err(HeaderError::FutureBlock {
                time: h.timestamp,
                now,
            });
        }
        self.parent_link(h, parent)?;
        verify_gas_limit(parent.gas_limit, h.gas_limit)?;
        if h.gas_used > h.gas_limit {
            return Err(HeaderError::GasUsedExceedsLimit {
                used: h.gas_used,
                limit: h.gas_limit,
            });
        }
        let extra = h.extra_fields_v2()?;
        let (round, qc) = (extra.round, &extra.quorum_cert);
        let mine_period = v2
            .config_at_round(round)
            .ok_or(HeaderError::NoV2Config(round))?
            .mine_period;
        // Strict `>`: the first V2 block, whose parent is the switch block, is exempt.
        if parent.number > v2.switch_block
            && parent.timestamp.wrapping_add(mine_period) > h.timestamp
        {
            return Err(HeaderError::InvalidTimestamp {
                time: h.timestamp,
                parent: parent.timestamp,
                period: mine_period,
            });
        }
        let qc_round = qc.proposed_block_info.round;
        if round <= qc_round {
            return Err(HeaderError::RoundInvalid { round, qc_round });
        }
        self.verify_qc(qc, parent, ctx, v2)?;

        if !nonce_ok(h) {
            return Err(HeaderError::InvalidVote);
        }
        if !h.mix_digest.is_zero() {
            return Err(HeaderError::InvalidMixDigest);
        }
        if h.uncle_hash != keccak256([alloy_rlp::EMPTY_LIST_CODE]) {
            return Err(HeaderError::InvalidUncleHash);
        }
        if self.spec.is_eip1559(h.number) {
            let want = U256::from(XDC_BASE_FEE)
                * U256::from(if self.spec.gas2500x.is_active(h.number) {
                    GAS_2500X_MULTIPLIER
                } else {
                    1
                });
            match h.base_fee {
                None => return Err(HeaderError::MissingBaseFee),
                Some(have) if have != want => {
                    return Err(HeaderError::InvalidBaseFee {
                        have: have.to_string(),
                        want: want.to_string(),
                    })
                }
                Some(_) => {}
            }
        }
        if h.difficulty != U256::from(1) {
            return Err(HeaderError::InvalidDifficulty {
                have: h.difficulty.as_limbs()[0] as i64,
                want: 1,
            });
        }

        let epoch_switch =
            qc.proposed_block_info.number == v2.switch_block || qc_round < round - round % epoch;
        if epoch_switch {
            if h.nonce.0 != [0u8; 8] {
                return Err(HeaderError::InvalidCheckpointVote);
            }
            if h.validators.is_empty() {
                return Err(HeaderError::EmptyEpochSwitchValidators);
            }
            if !h.validators.len().is_multiple_of(Address::len_bytes()) {
                return Err(HeaderError::InvalidCheckpointSigners);
            }
            let listed = addresses(&h.validators);
            if !same_set(&ctx.masternodes, &listed) {
                return Err(HeaderError::ValidatorsNotLegit);
            }
            if self.spec.validator_order.is_active(h.number) && ctx.masternodes != listed {
                return Err(HeaderError::ValidatorsOrder);
            }
            if !same_set(&ctx.penalties, &addresses(&h.penalties)) {
                return Err(HeaderError::PenaltiesNotLegit);
            }
        } else if !h.validators.is_empty() || !h.penalties.is_empty() {
            return Err(HeaderError::InvalidFieldInNonEpochSwitch);
        }

        // verifyMsgSignature over the masternodes of the block's epoch.
        if ctx.masternodes.is_empty() {
            return Err(HeaderError::EmptyMasternodes);
        }
        let validator = ecrecover(&h.v2_sig_hash(), &h.validator)?;
        if !ctx.masternodes.contains(&validator) {
            return Err(HeaderError::ValidatorNotWithinMasternodes(validator));
        }
        if validator != h.coinbase {
            return Err(HeaderError::CoinbaseAndValidatorMismatch {
                coinbase: h.coinbase,
                validator,
            });
        }
        let leader = (round % epoch % ctx.masternodes.len() as u64) as usize;
        if ctx.masternodes[leader] != validator {
            return Err(HeaderError::NotItsTurn(validator));
        }
        Ok(())
    }

    /// geth `verifyQC` (`engine_v2/utils.go:315`). Where geth tolerates a gap number that
    /// disagrees with the epoch switch because a snapshot matches, this rejects.
    fn verify_qc(
        &self,
        qc: &QuorumCert,
        parent: &Header,
        ctx: &V2Context,
        v2: &V2,
    ) -> Result<(), HeaderError> {
        let x = &self.spec.xdpos;
        let info = &qc.proposed_block_info;
        let masternodes = if parent.number == v2.switch_block {
            // The switch block lists its masternodes in `extra_data`, as a V1 checkpoint.
            let end = parent.extra_data.len().saturating_sub(EXTRA_SEAL);
            addresses(parent.extra_data.get(EXTRA_VANITY..end).unwrap_or_default())
        } else {
            ctx.parent_masternodes.clone().unwrap_or_default()
        };
        if masternodes.is_empty() || info.hash != parent.hash() {
            return Err(HeaderError::QcEpochInfo);
        }

        let digest = keccak256(alloy_rlp::encode(VoteForSign {
            proposed_block_info: info,
            gap_number: qc.gap_number,
        }));
        let mut seen_bytes = BTreeSet::new();
        let mut signers = BTreeSet::new();
        for sig in &qc.signatures {
            if !seen_bytes.insert(sig.clone()) {
                continue;
            }
            let who = ecrecover(&digest, sig).map_err(|_| HeaderError::QcSignatureUnrecoverable)?;
            if EcdsaSignature::from_slice(&sig[..64])
                .ok()
                .and_then(|s| s.normalize_s())
                .is_some()
            {
                return Err(HeaderError::QcSignatureHighS);
            }
            if !masternodes.contains(&who) {
                return Err(HeaderError::QcSignerNotMasternode(who));
            }
            signers.insert(who);
        }
        let threshold = v2
            .config_at_round(info.round)
            .ok_or(HeaderError::NoV2Config(info.round))?
            .cert_threshold;
        let need = masternodes.len() as f64 * threshold;
        if info.round > 0 && (signers.len() as f64) < need {
            return Err(HeaderError::QcBelowThreshold {
                unique: signers.len(),
                required: need.ceil() as usize,
            });
        }

        let base = parent_epoch_base(ctx.parent_epoch_switch, x.epoch);
        let expected = base.saturating_sub(x.gap);
        if expected != qc.gap_number {
            return Err(HeaderError::QcGapNumber {
                expected,
                got: qc.gap_number,
            });
        }

        // VerifyBlockInfo against the parent.
        if info.number != parent.number {
            return Err(HeaderError::QcBlockNumber {
                want: parent.number,
                got: info.number,
            });
        }
        if info.number == v2.switch_block {
            return match info.round {
                0 => Ok(()),
                r => Err(HeaderError::QcSwitchRound(r)),
            };
        }
        let want = parent.extra_fields_v2()?.round;
        if want != info.round {
            return Err(HeaderError::QcRound {
                want,
                got: info.round,
            });
        }
        Ok(())
    }
}

fn parent_epoch_base(switch: u64, epoch: u64) -> u64 {
    switch - switch % epoch
}

/// geth `ExtractValidatorsFromBytes` (`xdpos.go:2732`): 4-byte ASCII decimal tokens, with
/// NUL padding trimmed from both ends; one bad token empties the list.
fn m2_tokens(bytes: &[u8]) -> Vec<i64> {
    let mut out = vec![];
    for chunk in bytes.chunks_exact(4) {
        let start = chunk.iter().position(|b| *b != 0);
        let end = chunk.iter().rposition(|b| *b != 0);
        let text = match (start, end) {
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
