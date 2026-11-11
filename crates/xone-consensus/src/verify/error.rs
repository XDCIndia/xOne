//! Typed rejection reasons. Each variant names the geth-XDC error it stands for;
//! the mapping with file and line is in `spec/consensus/`. [`HeaderError::code`]
//! and [`BodyError::code`] give the variant name, which the corpus stores.
#![allow(
    clippy::disallowed_methods,
    reason = "error display only; no consensus effect"
)]

use alloy_primitives::{Address, B256};
use thiserror::Error;
use xone_primitives::{ExtraV2Error, SealError};

fn code_of(debug: String) -> String {
    debug
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HeaderError {
    #[error("the genesis block is not verified by this rule set")]
    Genesis,
    #[error("the V2 rules apply but the chain spec has no V2 section")]
    NoV2Section,
    #[error("the context kind does not match the consensus version of block {0}")]
    WrongContext(u64),
    #[error("header sets a post-London field that XDPoS does not allow here")]
    UnsealedFields,
    #[error("block time {time} is after now {now}")]
    FutureBlock { time: u64, now: u64 },
    #[error("beneficiary in checkpoint block is not zero")]
    CheckpointBeneficiary,
    #[error("vote nonce is neither all zero nor all ones")]
    InvalidVote,
    #[error("vote nonce in checkpoint block is not zero")]
    InvalidCheckpointVote,
    #[error("extra data is shorter than the 32 byte vanity")]
    MissingVanity,
    #[error("extra data is shorter than vanity plus the 65 byte seal")]
    MissingSignature,
    #[error("non-checkpoint block carries a signer list")]
    ExtraSigners,
    #[error("invalid signer list on checkpoint block")]
    InvalidCheckpointSigners,
    #[error("mix digest is not zero")]
    InvalidMixDigest,
    #[error("uncle hash is not the empty-list hash")]
    InvalidUncleHash,
    #[error("parent is not block {0} with the header's parent hash")]
    UnknownAncestor(u64),
    #[error("timestamp {time} is below parent {parent} plus period {period}")]
    InvalidTimestamp { time: u64, parent: u64, period: u64 },
    #[error("invalid difficulty: have {have}, want {want}")]
    InvalidDifficulty { have: i64, want: i64 },
    #[error("seal signature: {0}")]
    Seal(#[from] SealError),
    #[error("signer {0} is not authorised")]
    Unauthorized(Address),
    #[error("V1 block past the first epoch has no validator signature")]
    NoValidatorSignature,
    #[error("validator signature is not a recoverable 65 byte signature")]
    InvalidValidatorSignature,
    #[error("wrong pair of creator {creator} and validator {validator}")]
    FailedDoubleValidation {
        creator: Address,
        validator: Address,
    },
    #[error("the V1 checkpoint header for the M2 assignment is absent or malformed")]
    M2Assignment,
    // V2
    #[error("no validator signature")]
    NoValidatorSignatureV2,
    #[error("validator signature has length {0}, want 65")]
    ValidatorSignatureLength(usize),
    #[error("invalid gas limit: have {have}, parent {parent}")]
    InvalidGasLimit { have: u64, parent: u64 },
    #[error("gas limit {0} is below the minimum")]
    GasLimitTooLow(u64),
    #[error("gas used {used} exceeds gas limit {limit}")]
    GasUsedExceedsLimit { used: u64, limit: u64 },
    #[error("invalid V2 extra data: {0}")]
    InvalidV2Extra(#[from] ExtraV2Error),
    #[error("round {round} is not above the quorum certificate round {qc_round}")]
    RoundInvalid { round: u64, qc_round: u64 },
    #[error("no V2 config for round {0}")]
    NoV2Config(u64),
    #[error("QC: epoch info of the parent is missing or the QC block hash is not the parent")]
    QcEpochInfo,
    #[error("QC: a signature does not recover")]
    QcSignatureUnrecoverable,
    #[error("QC: signature has a high s value")]
    QcSignatureHighS,
    #[error("QC: signer {0} is not a masternode of the QC's epoch")]
    QcSignerNotMasternode(Address),
    #[error("QC: {unique} distinct signers, {required} required")]
    QcBelowThreshold { unique: usize, required: usize },
    #[error("QC: gap number {got}, expected {expected}")]
    QcGapNumber { expected: u64, got: u64 },
    #[error("QC: block number {got} is not the parent {want}")]
    QcBlockNumber { want: u64, got: u64 },
    #[error("QC: the V2 switch block has round 0, got {0}")]
    QcSwitchRound(u64),
    #[error("QC: round {got} is not the parent's round {want}")]
    QcRound { want: u64, got: u64 },
    #[error("missing base fee")]
    MissingBaseFee,
    #[error("invalid base fee: have {have}, want {want}")]
    InvalidBaseFee { have: String, want: String },
    #[error("empty validator list in epoch switch")]
    EmptyEpochSwitchValidators,
    #[error("epoch-switch validators are not the masternodes of the epoch")]
    ValidatorsNotLegit,
    #[error("epoch-switch validators are in a different order than the masternodes")]
    ValidatorsOrder,
    #[error("epoch-switch penalties are not the penalties of the epoch")]
    PenaltiesNotLegit,
    #[error("validators or penalties set in a block that is not an epoch switch")]
    InvalidFieldInNonEpochSwitch,
    #[error("empty masternode list")]
    EmptyMasternodes,
    #[error("validator {0} is not among the masternodes")]
    ValidatorNotWithinMasternodes(Address),
    #[error("coinbase {coinbase} is not the validator {validator}")]
    CoinbaseAndValidatorMismatch {
        coinbase: Address,
        validator: Address,
    },
    #[error("not the turn of {0}")]
    NotItsTurn(Address),
}

impl HeaderError {
    pub fn code(&self) -> String {
        code_of(format!("{self:?}"))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BodyError {
    #[error("uncles are not allowed")]
    UnclesNotAllowed,
    #[error("uncle hash: header {header}, computed {computed}")]
    UncleHashMismatch { header: B256, computed: B256 },
    #[error("transaction root: header {header}, computed {computed}")]
    TxRootMismatch { header: B256, computed: B256 },
    #[error("a transaction does not decode: {0}")]
    TxDecode(String),
    #[error("too many signing transactions: {count} > {max}")]
    TooManySigningTxs { count: usize, max: u64 },
    #[error("withdrawals missing in the body")]
    WithdrawalsMissing,
    #[error("withdrawals present in the body but not in the header")]
    WithdrawalsUnexpected,
    #[error("withdrawals root: header {header}, computed {computed}")]
    WithdrawalsRootMismatch { header: B256, computed: B256 },
    #[error("blob gas used {header} does not match {blobs} blobs")]
    BlobGasMismatch { header: u64, blobs: u64 },
    #[error("blobs present but the header has no blob gas used")]
    BlobsUnexpected,
    #[error("block access list hash missing at or after Amsterdam")]
    AccessListHashMissing,
    #[error("block access list hash set before Amsterdam")]
    AccessListHashUnexpected,
    #[error("block of {0} bytes is over the size cap")]
    BlockOversized(usize),
    #[error("{receipts} receipts for {txs} transactions")]
    ReceiptCount { receipts: usize, txs: usize },
    #[error("a receipt does not decode: {0}")]
    ReceiptDecode(String),
    #[error("bloom: header {header}, computed {computed}")]
    BloomMismatch { header: String, computed: String },
    #[error("receipt root: header {header}, computed {computed}")]
    ReceiptRootMismatch { header: B256, computed: B256 },
}

impl BodyError {
    pub fn code(&self) -> String {
        code_of(format!("{self:?}"))
    }
}
