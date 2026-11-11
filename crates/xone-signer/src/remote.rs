//! Remote-signer client interface and an in-process test double. No network here: the
//! transport is a later item. Only the safety unit may call a [`RemoteSigner`].
//!
//! The fencing token is a monotonically increasing `u64` that the signer ratchets: it
//! refuses any token below the highest it has seen. ADR-010 section 8 decided on a quiet
//! window instead of a token and says a lease "is another gate in front of `Fence::open`";
//! #40 asks for a token on the wire, so its shape here is provisional.

use std::sync::atomic::{AtomicU64, Ordering};

use alloy_primitives::B256;
use thiserror::Error;
use xone_primitives::{AlgorithmId, CryptoError, Domain, Signature, Signer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FencingToken(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignRequest {
    pub domain: Domain,
    pub digest: B256,
    pub fence: FencingToken,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Refusal {
    #[error("stale fencing token {presented:?}; the signer has seen {current:?}")]
    StaleFence {
        presented: FencingToken,
        current: FencingToken,
    },
    #[error("signing failed: {0}")]
    Backend(#[from] CryptoError),
}

pub trait RemoteSigner {
    fn algorithm(&self) -> AlgorithmId;
    fn sign(&self, req: &SignRequest) -> Result<Signature, Refusal>;
}

/// Test double: wraps any local [`Signer`] and enforces the fence like a real remote would.
#[derive(Debug)]
pub struct InProcessSigner<S> {
    inner: S,
    highest: AtomicU64,
}

impl<S: Signer> InProcessSigner<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            highest: AtomicU64::new(0),
        }
    }
}

impl<S: Signer> RemoteSigner for InProcessSigner<S> {
    fn algorithm(&self) -> AlgorithmId {
        self.inner.algorithm()
    }

    fn sign(&self, req: &SignRequest) -> Result<Signature, Refusal> {
        // A newer token fences out every older holder before it signs.
        let seen = self.highest.fetch_max(req.fence.0, Ordering::SeqCst);
        if req.fence.0 < seen {
            return Err(Refusal::StaleFence {
                presented: req.fence,
                current: FencingToken(seen),
            });
        }
        Ok(self.inner.sign(req.domain, &req.digest)?)
    }
}
