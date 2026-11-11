#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test support: a panic is the failure signal"
)]
//! A small in-memory chain for the BFT step machine, real secp256k1 keys, and certificate builders.

use std::{collections::BTreeMap, collections::HashMap, sync::Arc};

use alloy_primitives::{keccak256, Address, Bytes, B256};
use k256::ecdsa::SigningKey;
use xone_chainspec::{ExpTimeoutConfig, V2Config, V2};
use xone_consensus::v2::{
    bft::{Chain, EpochFacts, HeaderFacts},
    cert::{timeout_digest, vote_digest},
    Error,
};
use xone_primitives::{BlockInfo, QuorumCert, TimeoutCert};

pub struct Key(pub SigningKey);

impl Key {
    pub fn new(n: u8) -> Self {
        Self(SigningKey::from_slice(&keccak256([b'k', n])[..]).unwrap())
    }
    pub fn address(&self) -> Address {
        let point = self.0.verifying_key().to_encoded_point(false);
        Address::from_slice(&keccak256(&point.as_bytes()[1..])[12..])
    }
    pub fn sign(&self, digest: &B256) -> Bytes {
        let (sig, id) = self.0.sign_prehash_recoverable(digest.as_slice()).unwrap();
        let mut b = sig.to_bytes().to_vec();
        b.push(id.to_byte());
        b.into()
    }
}

/// `n` validators, their keys and a V2 config whose switch block is 100.
pub fn keys(n: u8) -> Vec<Key> {
    (1..=n).map(Key::new).collect()
}

pub fn v2(rows: &[(u64, V2Config)]) -> V2 {
    V2 {
        switch_block: 100,
        switch_epoch: 1,
        configs: rows.iter().cloned().collect::<BTreeMap<_, _>>(),
    }
}

pub fn row(timeout_period: u64, sync: u64, mine: u64) -> V2Config {
    V2Config {
        mine_period: mine,
        timeout_sync_threshold: sync,
        timeout_period,
        cert_threshold: 0.667,
        max_masternodes: 108,
        exp_timeout: ExpTimeoutConfig {
            base: 1.0,
            max_exponent: 0,
        },
        ..V2Config::default()
    }
}

/// One epoch for every block: all masternodes, gap 0.
pub struct TestChain {
    pub headers: HashMap<B256, Arc<HeaderFacts>>,
    pub head: Option<B256>,
    pub masternodes: Vec<Address>,
    pub candidates: Arc<[Address]>,
    pub gap: u64,
}

impl TestChain {
    pub fn new(masternodes: Vec<Address>) -> Self {
        Self {
            headers: HashMap::new(),
            head: None,
            candidates: masternodes.clone().into(),
            masternodes,
            gap: 0,
        }
    }

    /// Adds a header; the hash is made from the fields and a salt, so siblings differ.
    pub fn add(
        &mut self,
        number: u64,
        parent: B256,
        round: u64,
        qc: Option<QuorumCert>,
        salt: u8,
    ) -> B256 {
        let hash = keccak256(
            [
                &number.to_be_bytes()[..],
                parent.as_slice(),
                &round.to_be_bytes(),
                &[salt],
            ]
            .concat(),
        );
        self.headers.insert(
            hash,
            Arc::new(HeaderFacts {
                number,
                hash,
                parent,
                round,
                qc,
            }),
        );
        if self.head.is_none_or(|h| self.headers[&h].number <= number) {
            self.head = Some(hash);
        }
        hash
    }

    pub fn info(&self, hash: B256) -> BlockInfo {
        let h = &self.headers[&hash];
        BlockInfo {
            hash,
            round: h.round,
            number: h.number,
        }
    }

    /// A QC of `info` signed by the validators `who` (indexes into `keys`).
    pub fn qc(&self, info: &BlockInfo, keys: &[Key], who: &[usize]) -> QuorumCert {
        let d = vote_digest(info, self.gap);
        QuorumCert {
            proposed_block_info: info.clone(),
            signatures: who.iter().map(|i| keys[*i].sign(&d)).collect(),
            gap_number: self.gap,
        }
    }

    pub fn tc(&self, round: u64, keys: &[Key], who: &[usize]) -> TimeoutCert {
        let d = timeout_digest(round, self.gap);
        TimeoutCert {
            round,
            signatures: who.iter().map(|i| keys[*i].sign(&d)).collect(),
            gap_number: self.gap,
        }
    }
}

impl Chain for TestChain {
    fn header(&self, hash: &B256) -> Option<Arc<HeaderFacts>> {
        self.headers.get(hash).cloned()
    }
    fn head(&self) -> Option<Arc<HeaderFacts>> {
        self.head.and_then(|h| self.header(&h))
    }
    fn epoch(&self, hash: &B256) -> Result<Arc<EpochFacts>, Error> {
        if !self.headers.contains_key(hash) {
            return Err(Error::MissingHeader { hash: *hash });
        }
        self.epoch_of_round(0)
    }
    fn epoch_of_round(&self, _: u64) -> Result<Arc<EpochFacts>, Error> {
        Ok(Arc::new(EpochFacts {
            masternodes: self.masternodes.clone(),
            gap: self.gap,
            switch: B256::ZERO,
        }))
    }
    fn candidates(&self, _: u64) -> Option<Arc<[Address]>> {
        Some(self.candidates.clone())
    }
    fn timeout_gap(&self, _: u64) -> Result<u64, Error> {
        Ok(self.gap)
    }
}
