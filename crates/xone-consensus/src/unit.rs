//! The safety unit (ADR-010 sections 6 and 10): the one owner of the validator key.
//!
//! Every signature of the node's validator key leaves through [`SafetyUnit`]. The key is moved into the
//! unit and has no accessor. A signature needs an [`Authorized`] token, which only this module can
//! build, and the token is built after the record is on disk:
//!
//! ```text
//! intent -> fence open? -> decide(record, intent) -> persist_intent (write + sync) -> Authorized -> sign
//! ```
//!
//! A retry of an intent the record already holds writes nothing and signs the same bytes again (the
//! signer is deterministic). A store error stops the unit for good: no later call signs (ADR-005
//! section 9). Layering (`ci/layering.py`): only `xone-consensus` and `xone-node` may depend on
//! `xone-signer`; `xone-builder` signs by asking the unit through [`Sealer`].
//!
//! The token cannot be made outside this module:
//!
//! ```compile_fail,E0451
//! use xone_consensus::unit::Authorized;
//! let _ = Authorized { digest: &Default::default(), domain: xone_primitives::Domain::SealV2 };
//! ```

use alloy_primitives::{Address, B256};
use xone_db::{
    safety::{SafetyStore, StoreError},
    Durability,
};
use xone_primitives::{CryptoError, Domain, Signature};
use xone_signer::{
    remote::{FencingToken, Refusal as RemoteRefusal, RemoteSigner, SignRequest},
    LocalSigner,
};

use crate::v2::safety::{
    decide, DecodeError, Fence, Intent, Kind, Refusal, SafetyRecord,
};

/// Why a call was not signed.
#[derive(Debug, thiserror::Error)]
pub enum UnitError {
    #[error("refused: {0}")]
    Refused(#[from] Refusal),
    #[error("the fence is closed: a start is unproven until the quiet window has passed")]
    FenceClosed,
    #[error("safety store: {0}")]
    Store(#[from] StoreError),
    #[error("safety record: {0}")]
    Record(#[from] DecodeError),
    #[error("the record is of {have}, the key is {want}")]
    Identity { have: Address, want: Address },
    #[error("the safety store was never initialised")]
    NotInitialised,
    #[error("the unit stopped after a store failure; restart the node")]
    Stopped,
    #[error("signer: {0}")]
    Sign(String),
}

/// What the signature is for, and proof that the record is durable. Built only by [`SafetyUnit`].
pub struct Authorized<'a> {
    digest: &'a B256,
    domain: Domain,
}

impl Authorized<'_> {
    pub fn digest(&self) -> &B256 {
        self.digest
    }
    pub fn domain(&self) -> Domain {
        self.domain
    }
}

/// A key behind the unit: the local key or a remote signer with its fencing token. Its `sign` takes an
/// [`Authorized`], so only the unit can call it.
pub trait Backend {
    fn address(&self) -> Address;
    fn sign(&self, auth: &Authorized<'_>) -> Result<Signature, UnitError>;
}

impl Backend for LocalSigner {
    fn address(&self) -> Address {
        LocalSigner::address(self)
    }
    fn sign(&self, a: &Authorized<'_>) -> Result<Signature, UnitError> {
        xone_primitives::Signer::sign(self, a.domain, a.digest).map_err(|e: CryptoError| UnitError::Sign(e.to_string()))
    }
}

/// A remote signer, with the fencing token of this holder and the address of its key. The signer ratchets
/// the token and refuses an older one (`xone_signer::remote`).
pub struct Fenced<R> {
    pub signer: R,
    pub fence: FencingToken,
    pub address: Address,
}

impl<R: RemoteSigner> Backend for Fenced<R> {
    fn address(&self) -> Address {
        self.address
    }
    fn sign(&self, a: &Authorized<'_>) -> Result<Signature, UnitError> {
        self.signer
            .sign(&SignRequest {
                domain: a.domain,
                digest: *a.digest,
                fence: self.fence,
            })
            .map_err(|e: RemoteRefusal| UnitError::Sign(e.to_string()))
    }
}

/// What the producer may ask of the unit. `xone-builder` sees only this trait.
pub trait Sealer {
    /// The address of the validator key.
    fn address(&self) -> Address;
    /// The highest block number or round the unit already signed as `kind` (a V2 round or a V1 height).
    fn signed_up_to(&self, kind: Kind) -> u64;
    /// Sign `intent.digest`: decided, persisted, then signed.
    fn sign(&mut self, intent: Intent) -> Result<Signature, UnitError>;
    /// The same for an intent of the BFT machine, whose record (its observations) is joined into the one written.
    /// A double without a record signs as `sign` does.
    fn sign_joined(&mut self, intent: Intent, _machine: &SafetyRecord) -> Result<Signature, UnitError> {
        self.sign(intent)
    }
    /// The M2 co-signature of a V1 block (the same digest as M1, `Domain::ValidatorV1`).
    fn cosign_v1(&mut self, number: u64, digest: B256) -> Result<Signature, UnitError>;
    /// A sign transaction (to 0x89): the key signs it, the record does not change (a transaction is
    /// bound to its nonce and carries no equivocation risk).
    fn sign_tx(&mut self, digest: &B256) -> Result<Signature, UnitError>;
}

pub struct SafetyUnit<St, B> {
    store: St,
    backend: B,
    rec: SafetyRecord,
    dur: Durability,
    fence: Fence,
    stopped: bool,
}

impl<St: SafetyStore, B: Backend> SafetyUnit<St, B> {
    /// Opens a store that was initialised. A record of another key is never adopted.
    pub fn open(mut store: St, backend: B, quiet_ticks: u32) -> Result<Self, UnitError> {
        let loaded = store.load()?.ok_or(UnitError::NotInitialised)?;
        let rec = SafetyRecord::decode(&loaded.record)?;
        let want = backend.address();
        if rec.identity != want {
            return Err(UnitError::Identity {
                have: rec.identity,
                want,
            });
        }
        Ok(Self {
            store,
            backend,
            rec,
            dur: Durability::Durable,
            fence: Fence::start(quiet_ticks),
            stopped: false,
        })
    }

    /// `xone signer init`: a zero record for the key, written once. For a private devnet the node
    /// calls this on its own; a public network refuses to mine in this slice.
    pub fn init(store: &mut St, identity: Address) -> Result<(), UnitError> {
        let zero = SafetyRecord::new(identity);
        store.persist_intent(&Intent::new(Kind::Vote, 0, B256::ZERO).to_bytes(), &zero.encode())?;
        Ok(())
    }

    pub fn open_or_init(mut store: St, backend: B, quiet_ticks: u32) -> Result<Self, UnitError> {
        if store.load()?.is_none() {
            Self::init(&mut store, backend.address())?;
        }
        Self::open(store, backend, quiet_ticks)
    }

    pub fn record(&self) -> &SafetyRecord {
        &self.rec
    }

    pub fn fence_mut(&mut self) -> &mut Fence {
        &mut self.fence
    }

    pub fn set_durability(&mut self, d: Durability) {
        self.dur = d;
    }

    /// A vote, timeout or proposal intent. `machine` is the record of the BFT machine: its observations
    /// (QCs, TC, lock) are joined into the record that is written, so neither copy lowers the other.
    pub fn sign_with(
        &mut self,
        intent: Intent,
        machine: Option<&SafetyRecord>,
    ) -> Result<Signature, UnitError> {
        let domain = match intent.kind {
            Kind::Vote => Domain::VoteV2,
            Kind::Timeout => Domain::TimeoutV2,
            Kind::Propose => Domain::SealV2,
            Kind::SealV1 => Domain::SealV1,
        };
        self.persist_then_sign(intent, domain, machine)
    }

    fn persist_then_sign(
        &mut self,
        intent: Intent,
        domain: Domain,
        machine: Option<&SafetyRecord>,
    ) -> Result<Signature, UnitError> {
        if self.stopped {
            return Err(UnitError::Stopped);
        }
        if !self.fence.open() {
            return Err(UnitError::FenceClosed);
        }
        let mut next = decide(&self.rec, &intent, self.dur)?;
        if let Some(m) = machine {
            next = next.join(m);
        }
        if next != self.rec {
            // the write and the sync come first; the token below exists only after they returned
            if let Err(e) = self
                .store
                .persist_intent(&intent.to_bytes(), &next.encode())
            {
                self.stopped = true;
                return Err(e.into());
            }
            self.rec = next;
        }
        let auth = Authorized {
            digest: &intent.digest,
            domain,
        };
        self.backend.sign(&auth)
    }
}

impl<St: SafetyStore, B: Backend> Sealer for SafetyUnit<St, B> {
    fn address(&self) -> Address {
        self.backend.address()
    }
    fn signed_up_to(&self, kind: Kind) -> u64 {
        self.rec.signed[match kind {
            Kind::Vote => 0,
            Kind::Timeout => 1,
            Kind::Propose => 2,
            Kind::SealV1 => 3,
        }]
        .round
    }
    fn sign(&mut self, intent: Intent) -> Result<Signature, UnitError> {
        self.sign_with(intent, None)
    }
    fn sign_joined(&mut self, intent: Intent, machine: &SafetyRecord) -> Result<Signature, UnitError> {
        self.sign_with(intent, Some(machine))
    }
    fn cosign_v1(&mut self, number: u64, digest: B256) -> Result<Signature, UnitError> {
        self.persist_then_sign(
            Intent::new(Kind::SealV1, number, digest),
            Domain::ValidatorV1,
            None,
        )
    }
    fn sign_tx(&mut self, digest: &B256) -> Result<Signature, UnitError> {
        if self.stopped {
            return Err(UnitError::Stopped);
        }
        self.backend.sign(&Authorized {
            digest,
            domain: Domain::SignTx,
        })
    }
}
