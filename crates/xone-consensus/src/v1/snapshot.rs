//! The V1 signer snapshot and its `apply` (geth `consensus/XDPoS/snapshot.go`, a Clique-style
//! voting snapshot). It holds the signer set, the creators of the last blocks (`recents`) and the
//! votes in flight. The signer set of an epoch is not voted: `UpdateMasternodes` replaces it with the
//! candidates of the 0x88 contract at the gap block ([`Snapshot::reseed`], `xdpos.go:2189`), and a
//! snapshot rebuilt without that step has the wrong signers (xOneGo `bf73400a`, Apothem block 1800).

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, B256};
use xone_primitives::Header;

use crate::verify::HeaderError;

/// geth `Vote` (`snapshot.go:32`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vote {
    pub signer: Address,
    pub block: u64,
    pub address: Address,
    pub authorize: bool,
}

/// geth `Tally` (`snapshot.go:40`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub authorize: bool,
    pub votes: usize,
}

/// geth `Snapshot` (`snapshot.go:46-56`). `signers` iterates ascending by address, which is
/// `GetSigners` (`snapshot.go:249-263`) and the header order of a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub number: u64,
    pub hash: B256,
    pub signers: BTreeSet<Address>,
    pub recents: BTreeMap<u64, Address>,
    pub votes: Vec<Vote>,
    pub tally: BTreeMap<Address, Tally>,
}

const NONCE_AUTH_VOTE: [u8; 8] = [0xff; 8];
const NONCE_DROP_VOTE: [u8; 8] = [0; 8];

impl Snapshot {
    /// geth `newSnapshot` (`snapshot.go:59-73`).
    pub fn new(number: u64, hash: B256, signers: impl IntoIterator<Item = Address>) -> Self {
        Self {
            number,
            hash,
            signers: signers.into_iter().collect(),
            recents: BTreeMap::new(),
            votes: vec![],
            tally: BTreeMap::new(),
        }
    }

    /// `GetSigners`: ascending by address.
    pub fn get_signers(&self) -> Vec<Address> {
        self.signers.iter().copied().collect()
    }

    /// The signer set replaced by `signers`, everything else kept (geth `UpdateMasternodes`,
    /// `xdpos.go:2185-2189`).
    pub fn reseed(&mut self, signers: impl IntoIterator<Item = Address>) {
        self.signers = signers.into_iter().collect();
    }

    /// geth `inturn` (`snapshot.go:266-272`): the signer at position `number % len(signers)`.
    /// A signer that is not in the set is never in turn. No signers is a typed error (geth divides
    /// by zero).
    pub fn inturn(&self, number: u64, signer: Address) -> Result<bool, HeaderError> {
        let len = u64::try_from(self.signers.len()).map_err(|_| HeaderError::MissingSigners)?;
        if len == 0 {
            return Err(HeaderError::MissingSigners);
        }
        let offset = self.signers.iter().take_while(|s| **s != signer).count();
        Ok(u64::try_from(offset).is_ok_and(|o| number % len == o))
    }

    fn cast(&mut self, address: Address, authorize: bool) -> bool {
        // geth `validVote`: voting to add a non-signer or to drop a signer.
        if self.signers.contains(&address) == authorize {
            return false;
        }
        self.tally
            .entry(address)
            .and_modify(|t| t.votes += 1)
            .or_insert(Tally {
                authorize,
                votes: 1,
            });
        true
    }

    fn uncast(&mut self, address: Address, authorize: bool) {
        let Some(t) = self.tally.get_mut(&address) else {
            return;
        };
        if t.authorize != authorize {
            return;
        }
        if t.votes > 1 {
            t.votes -= 1;
        } else {
            self.tally.remove(&address);
        }
    }

    /// geth `apply` (`snapshot.go:163-247`): the snapshot after `headers`, which must be
    /// consecutive and start right after `self`. At a checkpoint (`number % epoch == 0`) the
    /// votes reset. Each header's creator (the seal) enters `recents`, which keeps the last
    /// `len(signers) / 2 + 1` blocks, and its coinbase and nonce are a vote.
    pub fn apply(&self, headers: &[Header], epoch: u64) -> Result<Self, HeaderError> {
        let Some(first) = headers.first() else {
            return Ok(self.clone());
        };
        if self.number.checked_add(1) != Some(first.number)
            || headers
                .windows(2)
                .any(|w| w[0].number.checked_add(1) != Some(w[1].number))
        {
            return Err(HeaderError::InvalidVotingChain);
        }
        let mut snap = self.clone();
        for header in headers {
            let number = header.number;
            if number.checked_rem(epoch).ok_or(HeaderError::ZeroEpoch)? == 0 {
                snap.votes.clear();
                snap.tally.clear();
            }
            snap.forget_old(number);
            let signer = header.v1_signer()?;
            snap.recents.insert(number, signer);

            if let Some(i) = snap
                .votes
                .iter()
                .position(|v| v.signer == signer && v.address == header.coinbase)
            {
                let v = snap.votes.remove(i);
                snap.uncast(v.address, v.authorize);
            }
            let authorize = match header.nonce.0 .0 {
                NONCE_AUTH_VOTE => true,
                NONCE_DROP_VOTE => false,
                _ => return Err(HeaderError::InvalidVote),
            };
            if snap.cast(header.coinbase, authorize) {
                snap.votes.push(Vote {
                    signer,
                    block: number,
                    address: header.coinbase,
                    authorize,
                });
            }

            let voted = header.coinbase;
            if let Some(t) = snap.tally.get(&voted).copied() {
                if t.votes > snap.signers.len() / 2 {
                    if t.authorize {
                        snap.signers.insert(voted);
                    } else {
                        snap.signers.remove(&voted);
                        snap.forget_old(number);
                        // The votes the removed signer cast are withdrawn.
                        while let Some(i) = snap.votes.iter().position(|v| v.signer == voted) {
                            let v = snap.votes.remove(i);
                            snap.uncast(v.address, v.authorize);
                        }
                    }
                    snap.votes.retain(|v| v.address != voted);
                    snap.tally.remove(&voted);
                }
            }
        }
        if let Some(last) = headers.last() {
            snap.number = last.number;
            snap.hash = last.hash();
        }
        Ok(snap)
    }

    /// `delete(snap.Recents, number - limit)` with `limit = len(signers)/2 + 1`
    /// (`snapshot.go:184-186`, `:224-226`).
    fn forget_old(&mut self, number: u64) {
        if let Ok(limit) = u64::try_from(self.signers.len() / 2 + 1) {
            if let Some(old) = number.checked_sub(limit) {
                self.recents.remove(&old);
            }
        }
    }
}
