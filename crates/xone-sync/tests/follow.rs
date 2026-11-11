#![allow(clippy::unwrap_used, clippy::expect_used, clippy::as_conversions)] // test code
//! Live follow over a fake peer set: pushed blocks, announcements, gap fill, the lying-TD peer,
//! empty, partial, duplicate and reordered answers, and the at-tip rules.

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_trie::EMPTY_ROOT_HASH;
use std::collections::VecDeque;
use xone_chainspec::{devnet, ChainSpec, Network};
use xone_consensus::verify::{Body, HeaderError};
use xone_primitives::Header;
use xone_state::{MemFlat, StateRead};
use xone_sync::{
    chain::{Chain, ChainRead},
    executor::{BlockExecutor, ExecError, Execution},
    follow::{at_tip_freshness_secs, Follower, PeerError, PeerInfo, PeerSource, Push, TIP_TICKS},
    store::Block,
    testkit::MemStore,
};

/// Executes nothing: returns what the header says. Follow logic is under test here, not execution
/// (the real executor runs in `tests/import.rs`).
struct Echo;

impl BlockExecutor for Echo {
    fn execute(&self, _: &dyn StateRead, h: &Header, _: &Body) -> Result<Execution, ExecError> {
        Ok(Execution {
            diff: Default::default(),
            receipts: vec![],
            state_root: h.state_root,
            gas_used: h.gas_used,
            access_list_hash: h.block_access_list_hash,
            aux: Default::default(),
        })
    }
}

type Accept = fn(&dyn ChainRead, &Header, &Header, u64) -> Result<(), HeaderError>;
type C = Chain<MemStore, Echo, Accept>;

fn accept(_: &dyn ChainRead, _: &Header, _: &Header, _: u64) -> Result<(), HeaderError> {
    Ok(())
}

fn spec() -> ChainSpec {
    let mut s = devnet();
    s.xdpos.v2 = None;
    s
}

fn header(parent: &Header, salt: u8) -> Header {
    Header {
        parent_hash: parent.hash(),
        number: parent.number + 1,
        timestamp: 1_000 + parent.number * 2,
        state_root: EMPTY_ROOT_HASH,
        tx_root: EMPTY_ROOT_HASH,
        receipt_root: EMPTY_ROOT_HASH,
        uncle_hash: keccak256([0xc0]),
        difficulty: U256::from(1),
        coinbase: Address::repeat_byte(salt),
        ..Default::default()
    }
}

fn genesis() -> Block {
    Block::new(
        Header {
            state_root: EMPTY_ROOT_HASH,
            tx_root: EMPTY_ROOT_HASH,
            receipt_root: EMPTY_ROOT_HASH,
            uncle_hash: keccak256([0xc0]),
            difficulty: U256::from(1),
            ..Default::default()
        },
        Body::default(),
    )
}

/// Blocks up to `n` on top of `base`; `salt` makes a sibling branch.
fn branch(base: &[Block], n: u64, salt: u8) -> Vec<Block> {
    let mut v = base.to_vec();
    while v.last().unwrap().number() < n {
        let h = header(&v.last().unwrap().header, salt);
        v.push(Block::new(h, Body::default()));
    }
    v
}

fn chain_at(blocks: &[Block], upto: u64) -> C {
    let g = &blocks[0];
    let c: C = Chain::new(
        spec(),
        MemStore::with_anchor(g.header.clone(), MemFlat::default(), 64, 1_000),
        Echo,
        accept as Accept,
    );
    for b in &blocks[1..=upto as usize] {
        c.insert(b.clone(), u64::MAX).unwrap();
    }
    c
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Honest,
    /// Answers every request with nothing.
    Empty,
    /// Headers in reverse order.
    Reordered,
    /// The first header twice.
    Duplicate,
    /// Only the first header and body of any answer.
    One,
}

struct Fake {
    id: u8,
    rank: u64,
    chain: Vec<Block>,
    mode: Mode,
    /// The head the status claims; `None`: the chain's last block.
    claimed: Option<B256>,
}

struct Net {
    peers: Vec<Fake>,
    pushes: VecDeque<Push<u8>>,
    /// (peer, request) in order.
    log: Vec<(u8, String)>,
}

impl Net {
    fn new(peers: Vec<Fake>) -> Self {
        Self {
            peers,
            pushes: VecDeque::new(),
            log: vec![],
        }
    }
    fn peer(&self, id: u8) -> &Fake {
        self.peers.iter().find(|p| p.id == id).unwrap()
    }
    fn asked(&self, id: u8) -> usize {
        self.log.iter().filter(|(p, _)| *p == id).count()
    }
}

impl PeerSource for Net {
    type Peer = u8;
    fn peers(&mut self) -> Vec<PeerInfo<u8>> {
        self.peers
            .iter()
            .map(|p| PeerInfo {
                peer: p.id,
                head: p.claimed.unwrap_or_else(|| p.chain.last().unwrap().hash()),
                rank: p.rank,
            })
            .collect()
    }
    fn poll(&mut self) -> Vec<Push<u8>> {
        self.pushes.drain(..).collect()
    }
    fn headers_by_number(
        &mut self,
        peer: &u8,
        from: u64,
        max: usize,
    ) -> Result<Vec<Header>, PeerError> {
        self.log.push((*peer, format!("headers #{from} x{max}")));
        let p = self.peer(*peer);
        if p.mode == Mode::Empty {
            return Ok(vec![]);
        }
        let mut hs: Vec<Header> = p
            .chain
            .iter()
            .skip(from as usize)
            .take(if p.mode == Mode::One { 1 } else { max })
            .map(|b| b.header.clone())
            .collect();
        match p.mode {
            Mode::Reordered => hs.reverse(),
            Mode::Duplicate if !hs.is_empty() => hs.insert(1.min(hs.len()), hs[0].clone()),
            _ => {}
        }
        Ok(hs)
    }
    fn header_by_hash(&mut self, peer: &u8, hash: B256) -> Result<Option<Header>, PeerError> {
        self.log.push((*peer, format!("header {hash}")));
        let p = self.peer(*peer);
        if p.mode == Mode::Empty {
            return Ok(None);
        }
        Ok(p.chain
            .iter()
            .find(|b| b.hash() == hash)
            .map(|b| b.header.clone()))
    }
    fn bodies(&mut self, peer: &u8, headers: &[Header]) -> Result<Vec<Body>, PeerError> {
        self.log.push((*peer, format!("bodies x{}", headers.len())));
        let p = self.peer(*peer);
        if p.mode == Mode::Empty {
            return Ok(vec![]);
        }
        Ok(headers
            .iter()
            .take(if p.mode == Mode::One { 1 } else { usize::MAX })
            .map_while(|h| p.chain.iter().find(|b| b.hash() == h.hash()))
            .map(|b| b.body.clone())
            .collect())
    }
}

fn honest(id: u8, rank: u64, chain: &[Block]) -> Fake {
    Fake {
        id,
        rank,
        chain: chain.to_vec(),
        mode: Mode::Honest,
        claimed: None,
    }
}

const NOW: u64 = 1_000_000; // far from every head: freshness plays no part unless a test sets it

fn follower() -> Follower<u8> {
    Follower::new(&spec())
}

#[test]
fn pushed_blocks_extend_the_head() {
    let all = branch(&[genesis()], 10, 1);
    let c = chain_at(&all, 5);
    let mut net = Net::new(vec![honest(1, 10, &all[..8])]);
    for b in &all[6..=7] {
        net.pushes.push_back(Push::NewBlock {
            peer: 1,
            block: Box::new(b.clone()),
            rank: 11,
        });
    }
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert_eq!((r.imported, r.rejected, r.stalled_at), (2, 0, None));
    assert_eq!(c.head(), (7, all[7].hash()));
    assert!(r.status.caught_up, "peer 1's head hash is ours");
    assert!(net.log.is_empty(), "nothing was requested: {:?}", net.log);
}

#[test]
fn announcements_are_fetched_and_imported() {
    let all = branch(&[genesis()], 10, 1);
    let c = chain_at(&all, 5);
    let mut net = Net::new(vec![honest(1, 10, &all[..7])]);
    net.pushes.push_back(Push::Announce {
        peer: 1,
        hash: all[6].hash(),
        number: 6,
    });
    // an announcement of a block we already hold costs no request
    net.pushes.push_back(Push::Announce {
        peer: 1,
        hash: all[3].hash(),
        number: 3,
    });
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert_eq!(
        c.head(),
        (6, all[6].hash()),
        "gap 6 is filled through the announced block"
    );
    assert_eq!(r.imported, 1, "{r:?}");
}

#[test]
fn a_gap_is_filled_by_header_and_body_requests() {
    let all = branch(&[genesis()], 30, 1);
    let c = chain_at(&all, 5);
    let mut net = Net::new(vec![honest(1, 10, &all)]);
    net.pushes.push_back(Push::NewBlock {
        peer: 1,
        block: Box::new(all[30].clone()),
        rank: 31,
    });
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert_eq!(c.head(), (30, all[30].hash()));
    assert_eq!(r.imported, 25);
    // one header request for 6..=29 and one body request for the 24 headers, not one per block
    let want = vec![
        (1, "headers #6 x24".to_string()),
        (1, "bodies x24".to_string()),
    ];
    assert_eq!(net.log, want);
}

#[test]
fn a_long_gap_is_filled_in_batches_of_at_most_128_blocks() {
    let all = branch(&[genesis()], 300, 1);
    let c = chain_at(&all, 1);
    let mut net = Net::new(vec![honest(1, 1, &all)]);
    net.pushes.push_back(Push::NewBlock {
        peer: 1,
        block: Box::new(all[300].clone()),
        rank: 1,
    });
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert_eq!(c.head().0, 300);
    assert_eq!(r.imported, 299);
    let asks: Vec<&str> = net.log.iter().map(|(_, m)| m.as_str()).collect();
    assert_eq!(
        asks,
        [
            "headers #2 x128",
            "bodies x128",
            "headers #130 x128",
            "bodies x128",
            "headers #258 x42",
            "bodies x42"
        ]
    );
}

#[test]
fn a_lying_td_peer_does_not_stall_the_follower() {
    // Peer 9 claims the largest TD there is and a head nobody has; it serves nothing. Peer 2 has a
    // TD of 1 and the real chain. TD only orders the asks: 9 is asked first, then 2.
    let all = branch(&[genesis()], 12, 1);
    let c = chain_at(&all, 4);
    let liar = Fake {
        id: 9,
        rank: u64::MAX,
        chain: vec![genesis()],
        mode: Mode::Empty,
        claimed: Some(B256::repeat_byte(0xee)),
    };
    let mut net = Net::new(vec![liar, honest(2, 1, &all)]);
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert_eq!(r.stalled_at, None);
    assert_eq!(
        c.head(),
        (12, all[12].hash()),
        "caught up to the honest peer"
    );
    assert!(r.status.caught_up);
    assert!(net.asked(9) >= 1, "the liar ranks first and is tried first");
    // and again: now the liar's unknown head costs one request per step, never a loop
    let before = net.log.len();
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert_eq!((r.imported, r.stalled_at), (0, None));
    assert!(net.log.len() - before <= 2, "{:?}", &net.log[before..]);
}

#[test]
fn a_high_td_peer_at_our_head_is_not_ahead() {
    // TD says peer 7 is far ahead. Its head hash is ours: we are caught up, and ask for nothing.
    let all = branch(&[genesis()], 8, 1);
    let c = chain_at(&all, 8);
    let mut net = Net::new(vec![honest(7, u64::MAX, &all)]);
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert!(r.status.caught_up);
    assert!(net.log.is_empty(), "{:?}", net.log);
}

#[test]
fn a_low_td_peer_that_is_ahead_is_followed() {
    // TD never gates: the peer with TD 0 holds the tip.
    let all = branch(&[genesis()], 9, 1);
    let c = chain_at(&all, 3);
    let mut net = Net::new(vec![honest(1, 0, &all)]);
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert_eq!(c.head().0, 9);
    assert!(r.status.caught_up);
}

#[test]
fn no_peer_serves_the_block_is_a_typed_stall() {
    let all = branch(&[genesis()], 9, 1);
    let c = chain_at(&all, 3);
    let empty = |id| Fake {
        mode: Mode::Empty,
        claimed: Some(all[9].hash()),
        ..honest(id, 5, &all)
    };
    let mut net = Net::new(vec![empty(1), empty(2)]);
    // both peers show their head hash (via the push) but cannot serve history
    net.pushes.push_back(Push::NewBlock {
        peer: 1,
        block: Box::new(all[9].clone()),
        rank: 10,
    });
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert_eq!(r.stalled_at, Some(4));
    assert_eq!(c.head().0, 3, "nothing moved");
    assert!(!r.status.caught_up);
    assert!(net.log.len() <= 8, "bounded requests: {:?}", net.log);
}

#[test]
fn reordered_duplicate_and_partial_answers_never_change_the_chain_wrongly() {
    for bad in [Mode::Reordered, Mode::Duplicate, Mode::One] {
        let all = branch(&[genesis()], 20, 1);
        let c = chain_at(&all, 2);
        // the bad peer ranks first; the honest one repairs
        let mut net = Net::new(vec![
            Fake {
                mode: bad,
                ..honest(1, 9, &all)
            },
            honest(2, 1, &all),
        ]);
        net.pushes.push_back(Push::NewBlock {
            peer: 1,
            block: Box::new(all[20].clone()),
            rank: 10,
        });
        let r = follower().step(&c, &mut net, NOW).unwrap();
        assert_eq!(
            c.head(),
            (20, all[20].hash()),
            "{}",
            match bad {
                Mode::Reordered => "reordered",
                Mode::Duplicate => "duplicate",
                _ => "partial",
            }
        );
        assert_eq!(
            r.rejected, 0,
            "a wrong answer is skipped before it reaches the chain"
        );
        if bad != Mode::One {
            assert!(net.asked(2) > 0, "the honest peer served");
        }
    }
}

#[test]
fn a_sibling_branch_is_connected_by_hash_and_adopted_by_fork_choice() {
    // V1 fork choice: the longer branch wins. Peer 1 pushes the head of a longer branch that
    // forks at block 3; the ancestors are fetched by hash.
    let main = branch(&[genesis()], 5, 1);
    let alt = branch(&main[..=3], 7, 2);
    let c = chain_at(&main, 5);
    let mut net = Net::new(vec![honest(1, 1, &alt)]);
    net.pushes.push_back(Push::NewBlock {
        peer: 1,
        block: Box::new(alt[7].clone()),
        rank: 8,
    });
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert_eq!(r.rejected, 0);
    assert_eq!(c.head(), (7, alt[7].hash()));
}

#[test]
fn pushed_blocks_beyond_q4_drop_the_oldest_and_the_gap_is_refilled() {
    let all = branch(&[genesis()], 60, 1);
    let c = chain_at(&all, 1);
    let mut net = Net::new(vec![honest(1, 1, &all)]);
    for b in &all[2..=40] {
        net.pushes.push_back(Push::NewBlock {
            peer: 1,
            block: Box::new(b.clone()),
            rank: 1,
        });
    }
    let mut f = follower();
    let r = f.step(&c, &mut net, NOW).unwrap();
    assert_eq!(f.dropped, 39 - 16, "the queue holds 16 blocks");
    assert_eq!(
        c.head().0,
        60,
        "the dropped head of the run is refetched as a gap, then the tip poll"
    );
    assert_eq!(r.rejected, 0);
}

#[test]
fn freshness_window_is_geth_at_tip_freshness_window() {
    assert_eq!(at_tip_freshness_secs(&spec()), 30, "V1 chains: the floor");
    for net in [Network::Mainnet, Network::Apothem] {
        let s = net.spec();
        let c = s
            .xdpos
            .v2
            .as_ref()
            .unwrap()
            .configs
            .values()
            .next_back()
            .unwrap();
        let want = (c.timeout_period * c.timeout_sync_threshold.max(1) + c.mine_period).max(30);
        assert_eq!(at_tip_freshness_secs(&s), want);
    }
    // mainnet: 60 s * 3 + 2 s
    assert_eq!(at_tip_freshness_secs(&Network::Mainnet.spec()), 182);
}

#[test]
fn at_tip_needs_three_steps_and_a_fresh_head_counts_without_peers() {
    let all = branch(&[genesis()], 4, 1);
    let c = chain_at(&all, 4);
    let head_time = all[4].header.timestamp;
    let mut net = Net::new(vec![]);
    let mut f = follower();
    // no peer at all, head 29 s old: fresh
    let mut last = None;
    for i in 0..TIP_TICKS {
        let r = f.step(&c, &mut net, head_time + 29).unwrap();
        assert!(r.status.head_fresh && !r.status.caught_up);
        assert_eq!(r.status.at_tip, i + 1 >= TIP_TICKS);
        last = Some(r);
    }
    assert!(last.unwrap().status.at_tip);
    // 31 s old: not fresh, the count restarts
    let r = f.step(&c, &mut net, head_time + 31).unwrap();
    assert!(!r.status.head_fresh && !r.status.at_tip);
}

#[test]
fn a_peer_at_our_head_does_not_make_a_node_far_behind_believe_it_is_caught_up() {
    // geth `peersConfirmAtTip` (`eth/sync_xdc.go:1759`): the hash matches but an announced block
    // is more than an epoch above us.
    let all = branch(&[genesis()], 4, 1);
    let c = chain_at(&all, 4);
    let mut net = Net::new(vec![honest(1, 1, &all)]);
    net.pushes.push_back(Push::Announce {
        peer: 1,
        hash: B256::repeat_byte(1),
        number: 4 + spec().xdpos.epoch + 1,
    });
    let r = follower().step(&c, &mut net, NOW).unwrap();
    assert!(!r.status.caught_up);
}
