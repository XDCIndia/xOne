//! What the reward and penalty engines read from the chain: headers, sign transactions and the
//! previous epochs' penalty lists, through traits, and the typed error for anything absent.
//!
//! Every read is keyed by hash and number together and is checked against the answer, so a block
//! of a side branch can never stand in for the canonical one (xOneGo `8be7d783`). An absent header
//! or body is [`Error::Missing`], never an empty list or a count of zero (`spec/consensus/rewards-v1.md`).

use alloy_primitives::{address, Address, B256};

/// geth `common.BlockSignersBinary`, `common/xdc_contracts.go:19`.
pub const BLOCK_SIGNERS: Address = address!("0000000000000000000000000000000000000089");
/// geth `common.HexSignMethod`, `common/types.go:66`: the selector of `sign(uint256,bytes32)`.
pub const SIGN_METHOD: [u8; 4] = [0xe3, 0x41, 0xea, 0xa4];
/// geth `common.MergeSignRange`, `common/types.go:76`: only every 15th block is signed for.
pub const MERGE_SIGN_RANGE: u64 = 15;
/// geth `common.RangeReturnSigner`, `common/constants.go:16`: the pre-upgrade comeback window.
pub const RANGE_RETURN_SIGNER: usize = 150;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("the {what} of block {number} ({hash}) is not available")]
    Missing {
        what: &'static str,
        number: u64,
        hash: B256,
    },
    #[error("block {number}: asked for {want}, the reader returned {got}")]
    NotCanonical { number: u64, want: B256, got: B256 },
    #[error("no second epoch switch below block {0}")]
    WindowNotFound(u64),
    #[error("block {0} is too low for a reward window")]
    ShortChain(u64),
    #[error("the window blocks are not contiguous from {start} at block {number}")]
    NotContiguous { start: u64, number: u64 },
    #[error("the holder snapshot is of state {got}, the parent state is {want}")]
    WrongState { want: B256, got: B256 },
    #[error("the holder snapshot has no entry for masternode {0}")]
    NoHolder(Address),
    #[error("no V2 config at round {0}")]
    NoV2Config(u64),
    #[error("the foundation wallet address is empty")]
    NoFoundation,
    #[error("the V2 switch block is not configured")]
    NoV2,
    #[error("block {0} is not above the V2 switch block")]
    NotV2(u64),
    #[error("reward amount out of range")]
    Amount,
    #[error("penalties differ: derived {derived:?}, header {header:?}")]
    PenaltyMismatch {
        derived: Vec<Address>,
        header: Vec<Address>,
    },
}

/// A transaction to 0x89 that is a sign transaction: exactly 68 bytes, the `sign(uint256,bytes32)`
/// selector, the signed block hash in the last 32 (geth `isSigningTx`, `consensus/XDPoS/reward.go:51`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignTx {
    pub sender: Address,
    pub signed: B256,
    /// Receipt status. Read only by the V1 count before `TIPSigning` (`reward.go:144`).
    pub ok: bool,
}

/// The hash a sign transaction signs, if `(to, input)` is one.
pub fn signed_hash(to: Option<Address>, input: &[u8]) -> Option<B256> {
    (to == Some(BLOCK_SIGNERS) && input.len() == 68 && input[..4] == SIGN_METHOD)
        .then(|| B256::from_slice(&input[36..]))
}

/// What a walk needs of a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderView {
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    /// V2: `Coinbase` (it equals the validator). V1: the recovered sealer.
    pub miner: Address,
    /// V2: `IsEpochSwitch` of the header. Unused for V1.
    pub epoch_switch: bool,
}

/// Canonical-chain reads by hash. `None` means the node does not have it.
pub trait BlockReader {
    fn header(&self, hash: B256, number: u64) -> Option<HeaderView>;
    /// The sign transactions of the block body, in block order.
    fn sign_txs(&self, hash: B256, number: u64) -> Option<Vec<SignTx>>;
}

/// The `Penalties` of earlier epoch-switch headers (geth `GetPreviousPenaltyByHash`,
/// `engine_v2/epochSwitch.go:26`).
pub trait PenaltyHistory {
    /// The list of the epoch switch `back` epochs before the epoch that contains `hash`
    /// (`back` = 0 is the switch of that epoch). `None` when a header on the way is absent.
    fn penalties(&self, hash: B256, back: usize) -> Option<Vec<Address>>;
}

/// The header at (`hash`, `number`), as asked.
pub fn header_at<R: BlockReader>(r: &R, hash: B256, number: u64) -> Result<HeaderView, Error> {
    let h = r.header(hash, number).ok_or(Error::Missing {
        what: "header",
        number,
        hash,
    })?;
    if h.hash != hash || h.number != number {
        return Err(Error::NotCanonical {
            number,
            want: hash,
            got: h.hash,
        });
    }
    Ok(h)
}

/// The sign transactions of block (`hash`, `number`); an absent body is an error.
pub fn body_at<R: BlockReader>(r: &R, hash: B256, number: u64) -> Result<Vec<SignTx>, Error> {
    r.sign_txs(hash, number).ok_or(Error::Missing {
        what: "body",
        number,
        hash,
    })
}

/// A block of a window with its sign transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedBlock {
    pub number: u64,
    pub hash: B256,
    pub signs: Vec<SignTx>,
}

/// Collects the blocks of a window along the parent chain of the block being finalized, so a side
/// block is never reached, and hands them out in ascending order.
pub struct SignTxCounter<'a, R> {
    reader: &'a R,
}

impl<'a, R: BlockReader> SignTxCounter<'a, R> {
    pub fn new(reader: &'a R) -> Self {
        Self { reader }
    }

    /// Blocks `first..=tip.1` of the chain that ends at `tip` = (hash, number), ascending, each
    /// with its body. Any absent header or body is [`Error::Missing`].
    pub fn collect(&self, tip: (B256, u64), first: u64) -> Result<Vec<SignedBlock>, Error> {
        let (mut hash, mut number) = tip;
        let mut out = Vec::new();
        while number >= first {
            let h = header_at(self.reader, hash, number)?;
            let signs = body_at(self.reader, hash, number)?;
            out.push(SignedBlock {
                number,
                hash,
                signs,
            });
            if number == 0 {
                break;
            }
            hash = h.parent_hash;
            number -= 1;
        }
        out.reverse();
        Ok(out)
    }
}
