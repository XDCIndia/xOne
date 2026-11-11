//! XDPoS V2: the signer safety unit ([`safety`]) and the follower rules as pure functions over
//! explicit inputs: no I/O, no clock, no global state. A driver supplies headers, state readers and
//! the penalty history; an absent input is a typed [`Error`], never an empty list. Rules and geth-XDC
//! lines: `spec/consensus/v2-*.md`.
//!
//! * [`cert`]: QC and TC signatures, threshold, the blank bootstrap TC;
//! * [`epoch_switch`]: switch detection by round, and the `Validators`/`Penalties` a switch carries;
//! * [`gap_snapshot`]: `SnapshotV2` from the 0x88 state at the gap block, the standby pool;
//! * [`masternodes`]: the per-round masternode list and the leader;
//! * [`commit`]: the 3-chain commit rule;
//! * [`bft`], [`pool`], [`timer`], [`syncinfo`], [`forensics`]: the vote path, the timeout path and the
//!   round timer as a step machine with typed effects (`spec/consensus/v2-bft-vote-path.md`).

pub mod bft;
pub mod cert;
pub mod commit;
pub mod epoch_switch;
pub mod forensics;
pub mod gap_snapshot;
pub mod masternodes;
pub mod pool;
pub mod safety;
pub mod syncinfo;
pub mod timer;

use alloy_primitives::{Address, B256};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("the V2 switch block is not configured")]
    NoV2,
    #[error("no V2 config at round {0}")]
    NoV2Config(u64),
    #[error("the epoch is zero")]
    ZeroEpoch,
    #[error("{what} of block {number} is not available")]
    Missing { what: &'static str, number: u64 },
    #[error("header {hash} is not available")]
    MissingHeader { hash: B256 },
    #[error("no masternodes")]
    NoMasternodes,
    #[error("signature does not recover a public key")]
    SignatureUnrecoverable,
    #[error("signature is not in low-S form")]
    SignatureHighS,
    #[error("signature of {0}, who is not a masternode of the epoch")]
    SignerNotMasternode(Address),
    #[error("{unique} distinct signers, {required} needed of {masternodes} masternodes")]
    BelowThreshold {
        unique: usize,
        required: usize,
        masternodes: usize,
    },
    #[error("the gap snapshot has no candidates")]
    SnapshotEmpty,
    #[error("certificate encoding: {0}")]
    Rlp(#[from] alloy_rlp::Error),
    #[error("round {0} has no leader: the masternode list is empty")]
    NoLeader(u64),
    #[error("too many masternodes: {0}")]
    TooMany(usize),
    #[error("block info does not match the header")]
    BlockInfoMismatch,
    #[error("gap number {got}, the epoch of the block has {expected}")]
    GapMismatch { expected: u64, got: u64 },
    #[error("{n} signatures, a peer certificate holds at most {cap}")]
    TooManySignatures { n: usize, cap: usize },
    #[error("penalties: {0}")]
    Penalty(#[from] crate::chain::Error),
}
