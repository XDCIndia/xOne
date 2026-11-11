//! The bootstrap anchor: a snapshot manifest that binds the snapshot to a chain, a genesis, a
//! checkpoint, a state root and a set of validators, and the proof that the validators committed to
//! the checkpoint. [`verify_anchor`] is pure: the caller passes the clock and the pins.
//!
//! A hash of the manifest file proves the bytes. It proves neither the chain nor the validators.
//! The proof here is the epoch-switch header that lists the masternodes, the headers from it to the
//! checkpoint, and the quorum certificates (V2) or seals (V1) above the checkpoint. The masternode
//! set is read from that header. Nothing in a header proves that set was elected, so one pin
//! (the checkpoint hash, or the switch header hash) must come from outside the manifest
//! (`spec/bootstrap-anchor.md`, "What is still trusted").

use std::collections::BTreeSet;

use alloy_primitives::{Address, B256};
use alloy_rlp::{Decodable, RlpDecodable, RlpEncodable};
use xone_chainspec::{ChainSpec, V2};
use xone_consensus::v1::checkpoint::{addresses, masternodes_of};
use xone_consensus::v2::{
    cert::verify_qc,
    commit::{commit_blocks, Headers, Link},
    epoch_switch::of_header,
    gap_snapshot::gap_number,
    Error as V2Error,
};
use xone_primitives::{BlockInfo, Header};

/// The manifest format version. A reader refuses any other (ADR-005 section 12 states the rule for
/// the datadir; this is the same rule for the snapshot).
pub const FORMAT_VERSION: u16 = 1;

/// A checkpoint is committed by a 3-chain: the checkpoint, two more blocks, and the QC of the last
/// (`commitBlocks`, `utils.go:530-598`), so the proof ends three blocks above it.
const COMMIT_DEPTH: u64 = 3;

/// Where the validators come from: the header that lists them (the epoch-switch header in V2,
/// the epoch checkpoint in V1) and the list as that header gives it.
#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Provenance {
    pub switch_number: u64,
    pub switch_hash: B256,
    pub validators: Vec<Address>,
}

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable, RlpDecodable)]
pub struct Anchor {
    pub version: u16,
    pub chain_id: u64,
    pub genesis_hash: B256,
    /// The checkpoint, which is the pivot of the snapshot.
    pub number: u64,
    pub hash: B256,
    pub state_root: B256,
    /// Seconds since the epoch, and how long the manifest says it stays usable.
    pub created_at: u64,
    pub max_age: u64,
    pub provenance: Provenance,
    /// Contiguous, from the provenance header to the end of the proof: V2 up to three blocks
    /// above the checkpoint (their QCs are the certificates), V1 as many as the seals need.
    pub headers: Vec<Header>,
}

/// What the verifier brings: the clock, the pins, and its own cap on the age of a manifest.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub now: u64,
    /// (number, hash) values known from outside the manifest, such as a release's checkpoint list
    /// or an operator flag.
    pub pins: &'a [(u64, B256)],
    pub max_age_cap: u64,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnchorError {
    #[error("manifest format version {found} is not supported (this build reads {supported})")]
    UnsupportedFormat { found: u16, supported: u16 },
    #[error("manifest encoding: {0}")]
    Encoding(#[from] alloy_rlp::Error),
    #[error("manifest is for chain {got}, this node runs chain {want}")]
    WrongChain { want: u64, got: u64 },
    #[error("manifest genesis {got} is not the genesis {want} of this chain")]
    WrongGenesis { want: B256, got: B256 },
    #[error("manifest is {age} s old, the limit is {max_age} s")]
    Stale { age: u64, max_age: u64 },
    #[error("manifest is dated {created_at}, after now ({now})")]
    FromFuture { created_at: u64, now: u64 },
    #[error("neither the checkpoint nor the validator header is pinned from outside the manifest")]
    NotPinned,
    #[error("the manifest has no headers")]
    EmptyProof,
    #[error("headers are not contiguous and linked at block {0}")]
    BrokenChain(u64),
    #[error("the validator header is not block {number} ({hash}) as the provenance says")]
    ProvenanceHeader { number: u64, hash: B256 },
    #[error("the validator list differs from the one in header {0}")]
    ProvenanceValidators(u64),
    #[error("the proof does not reach checkpoint {0}")]
    MissingCheckpoint(u64),
    #[error("header {got} is not the checkpoint {want}")]
    CheckpointHash { want: B256, got: B256 },
    #[error("state root {got} of the checkpoint header is not the manifest root {want}")]
    RootMismatch { want: B256, got: B256 },
    #[error("block {0} is not an epoch switch")]
    NotEpochSwitch(u64),
    #[error("block {0} is an epoch switch inside the proof, whose certificates are of one epoch")]
    EpochSwitchInProof(u64),
    #[error("the proof ends at block {have}, it needs block {need}")]
    TooShort { have: u64, need: u64 },
    #[error("header {number}: {source}")]
    Extra {
        number: u64,
        source: xone_primitives::ExtraV2Error,
    },
    #[error("the QC of block {0} does not certify its parent")]
    QcBlockMismatch(u64),
    #[error("gap number {got} in a QC, the epoch's is {want}")]
    GapNumber { want: u64, got: u64 },
    #[error("no V2 config for round {0}")]
    NoConfig(u64),
    #[error("forged certificate: {0}")]
    ForgedCertificate(V2Error),
    #[error("{unique} distinct signers, {required} needed of {masternodes}")]
    UnderThreshold {
        unique: usize,
        required: usize,
        masternodes: usize,
    },
    #[error("the checkpoint is not committed by the 3-chain of the proof")]
    NotCommitted,
    #[error(transparent)]
    Consensus(V2Error),
    #[error("V1 checkpoint {0} is not an epoch checkpoint")]
    V1NotCheckpoint(u64),
    #[error("V1 anchor {0} is the epoch checkpoint or outside its epoch")]
    V1Position(u64),
    #[error("block {0} has no seal that recovers")]
    V1Seal(u64),
    #[error("block {number} is sealed by {signer}, who is not in the checkpoint list")]
    V1Unauthorised { number: u64, signer: Address },
    #[error("{distinct} distinct sealers above the checkpoint, {required} needed")]
    V1Depth { distinct: usize, required: usize },
}

impl Anchor {
    pub fn encode(&self) -> Vec<u8> {
        alloy_rlp::encode(self)
    }

    /// Decodes a manifest. The version is the first field and is read first, so a manifest of a
    /// later format is refused as such and not as an encoding error of its newer fields.
    pub fn decode(buf: &[u8]) -> Result<Self, AnchorError> {
        let mut head = buf;
        alloy_rlp::Header::decode(&mut head)?;
        let found = u16::decode(&mut head)?;
        if found != FORMAT_VERSION {
            return Err(AnchorError::UnsupportedFormat {
                found,
                supported: FORMAT_VERSION,
            });
        }
        Ok(alloy_rlp::decode_exact(buf)?)
    }
}

/// Order: format, chain, genesis, freshness, pin, then the binding of the headers to the manifest
/// fields, then the certificates (V2) or seals (V1). The cheap refusals come first.
pub fn verify_anchor(spec: &ChainSpec, a: &Anchor, ctx: &Context<'_>) -> Result<(), AnchorError> {
    if a.version != FORMAT_VERSION {
        return Err(AnchorError::UnsupportedFormat {
            found: a.version,
            supported: FORMAT_VERSION,
        });
    }
    if a.chain_id != spec.chain_id {
        return Err(AnchorError::WrongChain {
            want: spec.chain_id,
            got: a.chain_id,
        });
    }
    if a.genesis_hash != spec.genesis_hash {
        return Err(AnchorError::WrongGenesis {
            want: spec.genesis_hash,
            got: a.genesis_hash,
        });
    }
    let age = ctx
        .now
        .checked_sub(a.created_at)
        .ok_or(AnchorError::FromFuture {
            created_at: a.created_at,
            now: ctx.now,
        })?;
    let max_age = a.max_age.min(ctx.max_age_cap);
    if age > max_age {
        return Err(AnchorError::Stale { age, max_age });
    }
    let p = &a.provenance;
    if !ctx
        .pins
        .iter()
        .any(|&pin| pin == (a.number, a.hash) || pin == (p.switch_number, p.switch_hash))
    {
        return Err(AnchorError::NotPinned);
    }

    let at = bind(a)?;
    match spec.xdpos.v2.as_ref().filter(|v| a.number > v.switch_block) {
        Some(v2) => v2_proof(spec, v2, a, at),
        None => v1_proof(spec, a, at),
    }
}

/// The headers are one linked run from the provenance header, and the manifest's checkpoint fields
/// are those of its header. Returns the index of the checkpoint.
fn bind(a: &Anchor) -> Result<usize, AnchorError> {
    let first = a.headers.first().ok_or(AnchorError::EmptyProof)?;
    let p = &a.provenance;
    if first.number != p.switch_number || first.hash() != p.switch_hash {
        return Err(AnchorError::ProvenanceHeader {
            number: p.switch_number,
            hash: p.switch_hash,
        });
    }
    for w in a.headers.windows(2) {
        if w[1].number != w[0].number.wrapping_add(1) || w[1].parent_hash != w[0].hash() {
            return Err(AnchorError::BrokenChain(w[1].number));
        }
    }
    let at = a
        .number
        .checked_sub(first.number)
        .and_then(|i| usize::try_from(i).ok())
        .ok_or(AnchorError::MissingCheckpoint(a.number))?;
    let cp = a
        .headers
        .get(at)
        .ok_or(AnchorError::MissingCheckpoint(a.number))?;
    let got = cp.hash();
    if got != a.hash {
        return Err(AnchorError::CheckpointHash { want: a.hash, got });
    }
    if cp.state_root != a.state_root {
        return Err(AnchorError::RootMismatch {
            want: a.state_root,
            got: cp.state_root,
        });
    }
    Ok(at)
}

/// The round of a V2 header; the V2 switch block is round 0 and has no V2 extra fields.
fn round_of(v2: &V2, h: &Header) -> Result<u64, AnchorError> {
    if h.number == v2.switch_block {
        return Ok(0);
    }
    h.extra_fields_v2()
        .map(|f| f.round)
        .map_err(|source| AnchorError::Extra {
            number: h.number,
            source,
        })
}

struct Run(Vec<Link>);

impl Headers for Run {
    fn link(&self, hash: B256) -> Option<Link> {
        self.0.iter().find(|l| l.hash == hash).copied()
    }
}

fn v2_proof(spec: &ChainSpec, v2: &V2, a: &Anchor, at: usize) -> Result<(), AnchorError> {
    let (e, g) = (spec.xdpos.epoch, spec.xdpos.gap);
    let sw = &a.headers[0];
    if !of_header(v2, e, sw).map_err(AnchorError::Consensus)?.0 {
        return Err(AnchorError::NotEpochSwitch(sw.number));
    }
    // In the first V2 epoch the switch header is the block above the V2 switch block: it carries
    // the list as `validators`. The V2 switch block is a V1 header and has none, so a proof that
    // starts on it is refused here.
    let set = addresses(&sw.validators);
    if set.is_empty() || set != a.provenance.validators {
        return Err(AnchorError::ProvenanceValidators(sw.number));
    }

    let need = a.number.saturating_add(COMMIT_DEPTH);
    let end =
        usize::try_from(need - sw.number).map_err(|_| AnchorError::MissingCheckpoint(a.number))?;
    if a.headers.len() <= end {
        return Err(AnchorError::TooShort {
            have: a.headers.last().map_or(0, |h| h.number),
            need,
        });
    }
    // The certificates above the checkpoint are signed by the masternodes of its epoch.
    for h in &a.headers[1..=end] {
        if of_header(v2, e, h).map_err(AnchorError::Consensus)?.0 {
            return Err(AnchorError::EpochSwitchInProof(h.number));
        }
    }
    let gap = gap_number(sw.number, e, g).map_err(AnchorError::Consensus)?;
    let mut links = Vec::with_capacity(end + 1);
    for (i, h) in a.headers[..=end].iter().enumerate() {
        links.push(Link {
            number: h.number,
            round: round_of(v2, h)?,
            hash: h.hash(),
            parent: h.parent_hash,
        });
        if i <= at {
            continue;
        }
        let qc = h
            .extra_fields_v2()
            .map_err(|source| AnchorError::Extra {
                number: h.number,
                source,
            })?
            .quorum_cert;
        let prev = &links[i - 1];
        let want = BlockInfo {
            hash: prev.hash,
            round: prev.round,
            number: prev.number,
        };
        if qc.proposed_block_info != want {
            return Err(AnchorError::QcBlockMismatch(h.number));
        }
        if qc.gap_number != gap {
            return Err(AnchorError::GapNumber {
                want: gap,
                got: qc.gap_number,
            });
        }
        let round = qc.proposed_block_info.round;
        let threshold = v2
            .config_at_round(round)
            .ok_or(AnchorError::NoConfig(round))?
            .cert_threshold;
        verify_qc(&qc, &set, threshold).map_err(|e| match e {
            V2Error::BelowThreshold {
                unique,
                required,
                masternodes,
            } => AnchorError::UnderThreshold {
                unique,
                required,
                masternodes,
            },
            other => AnchorError::ForgedCertificate(other),
        })?;
    }
    // The QC in the last header certifies the block two above the checkpoint: that commits it.
    let proposed = links[end - 1];
    let run = Run(links);
    match commit_blocks(&run, v2.switch_block, None, &proposed) {
        Ok(Some(c)) if c.hash == a.hash => Ok(()),
        Ok(_) => Err(AnchorError::NotCommitted),
        Err(e) => Err(AnchorError::Consensus(e)),
    }
}

fn v1_proof(spec: &ChainSpec, a: &Anchor, at: usize) -> Result<(), AnchorError> {
    let e = spec.xdpos.epoch;
    let cp = &a.headers[0];
    if e == 0 || !cp.number.is_multiple_of(e) {
        return Err(AnchorError::V1NotCheckpoint(cp.number));
    }
    // A block of the epoch is sealed by the list in the epoch's checkpoint (`v1-snapshot.md`).
    let last = a.headers.last().map_or(0, |h| h.number);
    if at == 0 || last >= cp.number + e {
        return Err(AnchorError::V1Position(a.number));
    }
    let list =
        masternodes_of(&cp.extra_data).map_err(|_| AnchorError::ProvenanceValidators(cp.number))?;
    if list.is_empty() || list != a.provenance.validators {
        return Err(AnchorError::ProvenanceValidators(cp.number));
    }
    let mut distinct = BTreeSet::new();
    for (i, h) in a.headers.iter().enumerate().skip(1) {
        let signer = h.v1_signer().map_err(|_| AnchorError::V1Seal(h.number))?;
        if !list.contains(&signer) {
            return Err(AnchorError::V1Unauthorised {
                number: h.number,
                signer,
            });
        }
        if i >= at {
            distinct.insert(signer);
        }
    }
    // The same two-thirds as the V2 certificate, `ceil(2n / 3)` sealers, in integers. This is a
    // rule of the anchor, not of geth: V1 has no certificate and no finality.
    let required = (2 * list.len()).div_ceil(3);
    if distinct.len() < required {
        return Err(AnchorError::V1Depth {
            distinct: distinct.len(),
            required,
        });
    }
    Ok(())
}
