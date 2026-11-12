#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: a panic is the failure signal"
)]
//! Three validators through the XB-15 step machine, deterministically (no clock, no sockets, one queue): in
//! every round only the scheduled leader proposes, the proposal is built and sealed by the producer through
//! the leader's safety unit, the three vote through their units, the QC forms, and the proposals commit by the
//! 3-chain rule. The mined round of a leader is the unit's record: a second proposal of a round is refused.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Arc,
};

use alloy_primitives::{keccak256, Address, Bloom, Bytes, B256};
use k256::ecdsa::SigningKey;
use xone_builder::producer::{
    build::{assemble, Roots},
    schedule::{self, Skip},
    seal_v2, timing,
};
use xone_chainspec::{ExpTimeoutConfig, V2Config, V2};
use xone_consensus::{
    unit::{Authorized, Backend, SafetyUnit, Sealer, UnitError},
    v2::{
        bft::{
            drive::{drive, Driven},
            Bft, Chain, Config, EpochFacts, Event, HeaderFacts,
        },
        cert::Secp,
        masternodes,
        safety::Kind,
        Error,
    },
};
use xone_db::{safety::MemStore, Durability};
use xone_primitives::{AlgorithmId, Header, Signature};

struct Key(SigningKey);

impl Backend for Key {
    fn address(&self) -> Address {
        let p = self.0.verifying_key().to_encoded_point(false);
        Address::from_slice(&keccak256(&p.as_bytes()[1..])[12..])
    }
    fn sign(&self, a: &Authorized<'_>) -> Result<Signature, UnitError> {
        let (s, id) = self
            .0
            .sign_prehash_recoverable(a.digest().as_slice())
            .unwrap();
        let mut b = s.to_bytes().to_vec();
        b.push(id.to_byte());
        Ok(Signature {
            algorithm: AlgorithmId::Secp256k1Keccak,
            bytes: Bytes::from(b),
        })
    }
}

fn key(n: u8) -> Key {
    Key(SigningKey::from_slice(&keccak256([b's', n])[..]).unwrap())
}

#[derive(Default)]
struct Net {
    facts: HashMap<B256, Arc<HeaderFacts>>,
    real: HashMap<B256, Header>,
    head: B256,
    list: Vec<Address>,
}

impl Net {
    fn add(&mut self, h: Header) -> B256 {
        let hash = h.hash();
        let f = h.extra_fields_v2().ok();
        self.facts.insert(
            hash,
            Arc::new(HeaderFacts {
                number: h.number,
                hash,
                parent: h.parent_hash,
                round: f.as_ref().map_or(0, |f| f.round),
                qc: f.map(|f| f.quorum_cert),
            }),
        );
        self.real.insert(hash, h);
        hash
    }
}

impl Chain for Net {
    fn header(&self, h: &B256) -> Option<Arc<HeaderFacts>> {
        self.facts.get(h).cloned()
    }
    fn head(&self) -> Option<Arc<HeaderFacts>> {
        self.header(&self.head)
    }
    fn epoch(&self, h: &B256) -> Result<Arc<EpochFacts>, Error> {
        if !self.facts.contains_key(h) {
            return Err(Error::MissingHeader { hash: *h });
        }
        self.epoch_of_round(0)
    }
    fn epoch_of_round(&self, _: u64) -> Result<Arc<EpochFacts>, Error> {
        Ok(Arc::new(EpochFacts {
            masternodes: self.list.clone(),
            gap: 0,
            switch: B256::ZERO,
        }))
    }
    fn candidates(&self, _: u64) -> Option<Arc<[Address]>> {
        Some(self.list.clone().into())
    }
    fn timeout_gap(&self, _: u64) -> Result<u64, Error> {
        Ok(0)
    }
}

struct Node {
    bft: Bft,
    unit: SafetyUnit<MemStore, Key>,
    committed: Vec<u64>,
}

const SWITCH: u64 = 100;
const EPOCH: u64 = 900;

fn v2() -> V2 {
    let row = V2Config {
        mine_period: 2,
        timeout_sync_threshold: 3,
        timeout_period: 10,
        cert_threshold: 0.667,
        max_masternodes: 108,
        exp_timeout: ExpTimeoutConfig {
            base: 1.0,
            max_exponent: 0,
        },
        ..V2Config::default()
    };
    V2 {
        switch_block: SWITCH,
        switch_epoch: 1,
        configs: BTreeMap::from([(0, row)]),
    }
}

/// Delivers events in a queue until it is empty; every signed vote goes to every node.
fn settle(net: &Net, nodes: &mut [Node], mut q: VecDeque<(usize, Event)>) {
    while let Some((i, ev)) = q.pop_front() {
        let fx = nodes[i].bft.step(net, Durability::Durable, ev);
        let n = &mut nodes[i];
        let d: Driven = drive(&n.bft, &mut n.unit, fx).unwrap();
        n.committed.extend(d.commits.iter().map(|l| l.number));
        for v in d.votes {
            for j in 0..nodes.len() {
                q.push_back((j, Event::Vote(v.clone(), (j != i).then_some(i as u64))));
            }
        }
    }
}

#[test]
fn three_validators_only_the_leader_proposes_and_every_proposal_commits() {
    const ROUNDS: u64 = 14;
    let mut net = Net::default();
    let units: Vec<SafetyUnit<MemStore, Key>> = (1..=3u8)
        .map(|n| SafetyUnit::open_or_init(MemStore::default(), key(n), 0).unwrap())
        .collect();
    net.list = units.iter().map(|u| u.address()).collect();
    let mut sw = Header {
        number: SWITCH,
        timestamp: 1_000,
        ..Header::default()
    };
    sw.extra_data = vec![0u8; 97].into();
    net.head = net.add(sw);
    let mut nodes: Vec<Node> = units
        .into_iter()
        .map(|unit| {
            let me = unit.address();
            let bft = Bft::new(
                Config {
                    v2: v2(),
                    gap: 450,
                    me: Some(me),
                },
                Secp,
                unit.record().clone(),
            )
            .unwrap();
            Node {
                bft,
                unit,
                committed: vec![],
            }
        })
        .collect();
    for i in 0..3 {
        settle(&net, &mut nodes, VecDeque::from([(i, Event::Synced)]));
    }
    let list = net.list.clone();
    let mut proposers: Vec<(u64, usize)> = vec![];
    for round in 1..=ROUNDS {
        assert!(
            nodes.iter().all(|n| n.bft.round() == round),
            "round {round}"
        );
        // the schedule: exactly one of the three is the leader
        let turn: Vec<Result<(), Skip>> = nodes
            .iter()
            .map(|n| schedule::v2_turn(&n.unit, round, EPOCH, &list))
            .collect();
        let leader = masternodes::leader_index(round, EPOCH, 3).unwrap();
        for (i, t) in turn.iter().enumerate() {
            match (i == leader, t) {
                (true, Ok(())) => {}
                (false, Err(Skip::NotLeader { leader: l })) => {
                    assert_eq!(l.as_slice(), list[leader].as_slice())
                }
                other => panic!("round {round} node {i}: {other:?}"),
            }
        }
        // the leader builds on the block of its highest QC, and seals through its unit
        let n = &mut nodes[leader];
        let qc = n.bft.highest_qc().clone();
        let parent = net.real[&qc.proposed_block_info.hash].clone();
        let time = timing::v2_block_time(parent.timestamp, 2, parent.timestamp + 2);
        let h = seal_v2::prepare(&seal_v2::Prepare {
            parent: &parent,
            round,
            qc: &qc,
            me: n.unit.address(),
            gas_limit: 420_000_000,
            base_fee: None,
            time,
            switch: None,
        })
        .unwrap();
        let h = assemble(
            h,
            &Roots {
                state_root: B256::with_last_byte(1),
                tx_root: B256::with_last_byte(2),
                receipt_root: B256::with_last_byte(3),
                bloom: Bloom::ZERO,
                gas_used: 0,
            },
        );
        let sealed = seal_v2::seal(h.clone(), round, &mut n.unit).unwrap();
        assert_eq!(sealed.v2_signer().unwrap(), n.unit.address());
        // the round is mined: the guard says so, and another block of the round is refused by the unit
        assert_eq!(
            schedule::v2_turn(&n.unit, round, EPOCH, &list),
            Err(Skip::AlreadyMined {
                round,
                highest: round
            })
        );
        let mut other = h;
        other.gas_used = 1;
        assert!(
            seal_v2::seal(other, round, &mut n.unit).is_err(),
            "a second block of the round is not signed"
        );
        proposers.push((round, leader));
        let hash = net.add(sealed);
        settle(
            &net,
            &mut nodes,
            (0..3).map(|i| (i, Event::Proposal(hash))).collect(),
        );
    }
    // one proposal per round, by the scheduled leader
    assert_eq!(proposers.len() as u64, ROUNDS);
    for (r, l) in &proposers {
        assert_eq!(*l, (r % EPOCH % 3) as usize);
    }
    // the proposals commit in order, the same on all three, the last two still waiting for their 3-chain
    let want: Vec<u64> = (1..=ROUNDS - 2).map(|k| SWITCH + k).collect();
    for n in &nodes {
        assert_eq!(n.committed, want);
    }
    // the unit of each leader holds the rounds it proposed; a vote or a timeout never sets it
    for (i, n) in nodes.iter().enumerate() {
        let mine = proposers
            .iter()
            .filter(|(_, l)| *l == i)
            .map(|(r, _)| *r)
            .max()
            .unwrap();
        assert_eq!(n.unit.signed_up_to(Kind::Propose), mine);
        assert!(
            n.unit.signed_up_to(Kind::Vote) >= ROUNDS - 1,
            "every node voted in the rounds"
        );
        assert!(
            n.unit.record().dominates(n.bft.record()) || n.bft.record().seq >= n.unit.record().seq
        );
    }
}
