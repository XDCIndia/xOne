#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::disallowed_methods,
    reason = "test code: vectors are trusted inputs and a panic is the failure signal"
)]
//! The V2 follower vectors of `conformance/vectors/v2/` (`README.md` there): inputs and expected
//! values come from the archives (`gap-*.json`, `leader-*.json`) or from the pinned geth-XDC V2
//! engine (`crates/xone-consensus/oracle/v2`: `sort`, `calc`, `epoch`, `cert`, `commit`, `midadd`).
//! Nothing is typed by hand.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use serde::Deserialize;
use xone_chainspec::{apothem, load_named, ChainSpec, V2Config, V2};
use xone_consensus::chain::{BlockReader, HeaderView, PenaltyHistory, SignTx};
use xone_consensus::penalty::same_set;
use xone_consensus::v2::{
    cert::{self, TcContext},
    commit::{self, Headers, Link},
    epoch_switch::{self, SwitchQuery},
    gap_snapshot::{self, SnapshotV2},
    masternodes, Error,
};
use xone_primitives::{BlockInfo, ExtraFieldsV2, QuorumCert};
use xone_state::{Account, StateRead};

fn read<T: for<'de> Deserialize<'de>>(file: &str) -> T {
    let path = format!(
        "{}/../../conformance/vectors/v2/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}")))
        .unwrap()
}

/// 0x88 storage as the vector wrote it.
struct Words(BTreeMap<B256, U256>);
impl Words {
    fn of(w: &[(B256, B256)]) -> Self {
        Self(
            w.iter()
                .map(|(k, v)| (*k, U256::from_be_bytes(v.0)))
                .collect(),
        )
    }
}
impl StateRead for Words {
    fn account(&self, _: Address) -> Option<Account> {
        None
    }
    fn storage(&self, _: Address, slot: B256) -> U256 {
        self.0.get(&slot).copied().unwrap_or_default()
    }
}

#[derive(Deserialize, Clone)]
struct CfgIn {
    epoch: u64,
    gap: u64,
    switch_block: u64,
    switch_epoch: u64,
    configs: BTreeMap<u64, CfgRow>,
}
#[derive(Deserialize, Clone)]
struct CfgRow {
    max_masternodes: u64,
    cert_threshold: f64,
    mine_period: u64,
}

/// Apothem's spec with the V2 section and epoch of an oracle case.
fn spec_of(c: &CfgIn) -> ChainSpec {
    let mut s = apothem();
    s.xdpos.epoch = c.epoch;
    s.xdpos.gap = c.gap;
    s.xdpos.v2 = Some(V2 {
        switch_block: c.switch_block,
        switch_epoch: c.switch_epoch,
        configs: c
            .configs
            .iter()
            .map(|(r, row)| {
                (
                    *r,
                    V2Config {
                        max_masternodes: row.max_masternodes,
                        cert_threshold: row.cert_threshold,
                        mine_period: row.mine_period,
                        ..V2Config::default()
                    },
                )
            })
            .collect(),
    });
    s
}

// ---------------------------------------------------------------- sort (0x88 reader + frozen sort)

#[derive(Deserialize)]
struct SortCase {
    id: String,
    words: Vec<(B256, B256)>,
    expected: Vec<Address>,
}

#[test]
fn the_pool_read_from_0x88_state_equals_geth_sort_candidates_by_stake() {
    let cases: Vec<SortCase> = read("sort.json");
    let widths: Vec<usize> = cases.iter().map(|c| c.expected.len()).collect();
    assert!(
        widths.contains(&463),
        "the 464-slot vector (one hole) is missing: {widths:?}"
    );
    let mut bad = vec![];
    for c in &cases {
        let got = gap_snapshot::build(&Words::of(&c.words), 7)
            .unwrap()
            .candidates;
        if got != c.expected {
            bad.push(c.id.clone());
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
    println!("sort vectors: {}", cases.len());
}

// ---------------------------------------------------------------- calcMasternodes and the standby pool

#[derive(Deserialize)]
struct CalcCase {
    id: String,
    cfg: CfgIn,
    number: u64,
    round: u64,
    pool: Option<Vec<Address>>,
    v1_list: Option<Vec<Address>>,
    penalties: Option<Vec<Address>>,
    masternodes: Option<Vec<Address>>,
    penalties_out: Option<Vec<Address>>,
    standby: Option<Vec<Address>>,
    error: Option<String>,
}

#[test]
fn masternodes_cap_and_standby_equal_geth_calc_masternodes() {
    let cases: Vec<CalcCase> = read("calc.json");
    let mut bad = vec![];
    for c in &cases {
        let spec = spec_of(&c.cfg);
        let (pool, v1, pens) = (
            c.pool.clone().unwrap_or_default(),
            c.v1_list.clone().unwrap_or_default(),
            c.penalties.clone().unwrap_or_default(),
        );
        let snap = if gap_snapshot::uses_v1_list(c.number, c.cfg.epoch, c.cfg.switch_block).unwrap()
        {
            let mut extra = vec![0u8; 32];
            extra.extend(v1.iter().flat_map(|a| a.0));
            extra.extend([0u8; 65]);
            gap_snapshot::bootstrap(&extra, c.cfg.switch_block)
        } else {
            Ok(SnapshotV2 {
                number: 0,
                candidates: pool,
            })
        };
        let q = SwitchQuery {
            number: c.number,
            parent: B256::ZERO,
            round: c.round,
            parent_round: 0,
            parent_masternodes: vec![],
        };
        let got = snap
            .and_then(|s| epoch_switch::lists(&spec, &q, &s, |_| Ok(pens.clone())).map(|e| (s, e)));
        match (&got, &c.error) {
            (Err(_), Some(_)) => {}
            (Ok((s, e)), None) => {
                let standby = gap_snapshot::standby(&s.candidates, &e.validators, &e.penalties);
                // Go writes an empty list as null
                let want = |l: &Option<Vec<Address>>| l.clone().unwrap_or_default();
                if e.validators != want(&c.masternodes)
                    || e.penalties != want(&c.penalties_out)
                    || standby != want(&c.standby)
                {
                    bad.push(c.id.clone());
                }
            }
            _ => bad.push(format!("{}: accept/reject differ", c.id)),
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
    assert!(cases
        .iter()
        .any(|c| c.pool.as_ref().is_some_and(|p| p.len() == 464)
            && c.masternodes.as_ref().is_some_and(|m| m.len() == 108)));
    println!("calc vectors: {}", cases.len());
}

// ---------------------------------------------------------------- epoch-switch detection

#[derive(Deserialize)]
struct EpochOut {
    cfg: CfgIn,
    headers: Vec<EpochCase>,
    at_round: Vec<AtRound>,
}
#[derive(Deserialize)]
struct EpochCase {
    number: u64,
    round: u64,
    qc_round: u64,
    qc_number: u64,
    is_switch: bool,
    epoch_num: u64,
    error: Option<String>,
}
#[derive(Deserialize)]
struct AtRound {
    round: u64,
    parent_number: u64,
    parent_round: u64,
    is_switch: bool,
    epoch_num: u64,
    error: Option<String>,
}

fn fields(round: u64, qc_round: u64, qc_number: u64) -> ExtraFieldsV2 {
    ExtraFieldsV2 {
        round,
        quorum_cert: QuorumCert {
            proposed_block_info: BlockInfo {
                hash: B256::ZERO,
                round: qc_round,
                number: qc_number,
            },
            signatures: vec![],
            gap_number: 0,
        },
    }
}

#[test]
fn epoch_switch_detection_equals_geth_is_epoch_switch() {
    let o: EpochOut = read("epoch.json");
    let spec = spec_of(&o.cfg);
    let v2 = spec.xdpos.v2.as_ref().unwrap();
    let mut bad = vec![];
    for c in &o.headers {
        let f = fields(c.round, c.qc_round, c.qc_number);
        let got = epoch_switch::is_epoch_switch(v2, o.cfg.epoch, c.number, Some(&f));
        if got.is_ok() != c.error.is_none()
            || got.as_ref().is_ok_and(|g| *g != (c.is_switch, c.epoch_num))
        {
            bad.push(format!(
                "{} r{} qc{}/{}",
                c.number, c.round, c.qc_round, c.qc_number
            ));
        }
    }
    for c in &o.at_round {
        let got = epoch_switch::is_epoch_switch_at_round(
            v2,
            o.cfg.epoch,
            c.round,
            c.parent_number,
            c.parent_round,
        );
        if got.is_ok() != c.error.is_none()
            || got.as_ref().is_ok_and(|g| *g != (c.is_switch, c.epoch_num))
        {
            bad.push(format!(
                "at_round {} parent {}/{}",
                c.round, c.parent_number, c.parent_round
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {}: {:?}",
        bad.len(),
        o.headers.len() + o.at_round.len(),
        &bad[..bad.len().min(10)]
    );
    // the vector holds both outcomes, and switches that a block-number rule would miss
    assert!(o.headers.iter().any(|c| c.is_switch && c.number % 900 != 0));
    println!("epoch vectors: {}", o.headers.len() + o.at_round.len());
}

// ---------------------------------------------------------------- QC, TC and SyncInfo

#[derive(Deserialize)]
struct CertOut {
    cases: Vec<CertCase>,
    decode: Vec<DecodeCase>,
    threshold_edges: Vec<Edge>,
}
#[derive(Deserialize)]
struct CertCase {
    id: String,
    kind: String,
    label: String,
    masternodes: Vec<Address>,
    threshold: f64,
    snapshot_candidates: usize,
    qc: Bytes,
    tc: Bytes,
    accept: bool,
}
#[derive(Deserialize)]
struct DecodeCase {
    id: String,
    #[serde(rename = "type")]
    ty: String,
    rlp: Bytes,
    accept: bool,
}
#[derive(Deserialize)]
struct Edge {
    n: usize,
    threshold: f64,
    min_signers: usize,
}

/// The `certificateThreshold` of the config at `round`: the rows of the oracle cases are
/// `{0: threshold, 100000: 0.9}`.
fn threshold_at(c: &CertCase, round: u64) -> f64 {
    let cfg = CfgIn {
        epoch: 900,
        gap: 450,
        switch_block: 1800,
        switch_epoch: 2,
        configs: [
            (
                0,
                CfgRow {
                    max_masternodes: 108,
                    cert_threshold: c.threshold,
                    mine_period: 2,
                },
            ),
            (
                100_000,
                CfgRow {
                    max_masternodes: 108,
                    cert_threshold: 0.9,
                    mine_period: 2,
                },
            ),
        ]
        .into(),
    };
    let spec = spec_of(&cfg);
    spec.xdpos
        .v2
        .as_ref()
        .unwrap()
        .config_at_round(round)
        .unwrap()
        .cert_threshold
}

fn kind_of(e: &Error) -> &'static str {
    match e {
        Error::BelowThreshold { .. } => "below_threshold",
        Error::SignerNotMasternode(_) => "non_member",
        Error::SignatureHighS => "high_s",
        Error::SignatureUnrecoverable => "unrecoverable",
        Error::SnapshotEmpty => "snapshot_missing",
        _ => "other",
    }
}

fn run_cert(c: &CertCase) -> Result<(), Error> {
    let tc_ctx = |tc_round| TcContext {
        snapshot_candidates: if c.label.starts_with("snapshot_missing") {
            0
        } else {
            c.snapshot_candidates
        },
        masternodes: &c.masternodes,
        threshold: threshold_at(c, tc_round),
    };
    match c.kind.as_str() {
        "qc" => {
            let qc = cert::decode_qc(&c.qc)?;
            cert::verify_qc(
                &qc,
                &c.masternodes,
                threshold_at(c, qc.proposed_block_info.round),
            )
        }
        "tc" => {
            let tc = cert::decode_tc(&c.tc)?;
            cert::verify_tc(&tc, &tc_ctx(tc.round))
        }
        "syncinfo" => {
            let qc = cert::decode_qc(&c.qc)?;
            cert::verify_qc(
                &qc,
                &c.masternodes,
                threshold_at(c, qc.proposed_block_info.round),
            )?;
            let tc = cert::decode_tc(&c.tc)?;
            cert::verify_sync_info_tc(&tc, &tc_ctx(tc.round))
        }
        k => panic!("{k}"),
    }
}

#[test]
fn qc_tc_and_sync_info_acceptance_equals_geth() {
    let o: CertOut = read("cert.json");
    let mut bad = vec![];
    let mut by_kind: BTreeMap<(String, bool), usize> = BTreeMap::new();
    for c in &o.cases {
        let got = run_cert(c);
        *by_kind.entry((c.kind.clone(), c.accept)).or_default() += 1;
        if got.is_ok() != c.accept {
            bad.push(format!("{}: geth accept={}, got {got:?}", c.id, c.accept));
            continue;
        }
        // the reason, where the case was built to fail for one
        let want = c.label.split(':').next().unwrap();
        if let Err(e) = &got {
            if matches!(
                want,
                "below_threshold" | "non_member" | "high_s" | "unrecoverable" | "snapshot_missing"
            ) && kind_of(e) != want
            {
                bad.push(format!("{}: want {want}, got {e}", c.id));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {}:\n{}",
        bad.len(),
        o.cases.len(),
        bad.join("\n")
    );
    println!("cert vectors: {} {by_kind:?}", o.cases.len());
}

#[test]
fn the_threshold_is_geths_float_comparison() {
    let o: CertOut = read("cert.json");
    for e in &o.threshold_edges {
        assert_eq!(
            cert::min_signers(e.n, e.threshold).unwrap(),
            e.min_signers,
            "{} x {}",
            e.n,
            e.threshold
        );
    }
    // 3 masternodes at 0.667 need all 3: 2.001 is above 2
    assert_eq!(cert::min_signers(3, 0.667).unwrap(), 3);
    assert!(o.threshold_edges.len() >= 20);
}

#[test]
fn certificate_decoding_accepts_exactly_what_geth_rlp_decodes() {
    let o: CertOut = read("cert.json");
    let mut bad = vec![];
    for d in &o.decode {
        let ok = match d.ty.as_str() {
            "tc" => cert::decode_tc(&d.rlp).is_ok(),
            _ => cert::decode_qc(&d.rlp).is_ok(),
        };
        if ok != d.accept {
            bad.push(d.id.clone());
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
    assert!(o
        .decode
        .iter()
        .any(|d| d.id.contains("trailing") && !d.accept));
}

// ---------------------------------------------------------------- 3-chain commit

#[derive(Deserialize)]
struct CommitOut {
    switch_block: u64,
    chains: Vec<Vec<L>>,
    cases: Vec<CommitCase>,
}
#[derive(Deserialize, Clone)]
struct L {
    number: u64,
    round: u64,
    hash: B256,
    parent: B256,
}
impl L {
    fn link(&self) -> Link {
        Link {
            number: self.number,
            round: self.round,
            hash: self.hash,
            parent: self.parent,
        }
    }
}
#[derive(Deserialize)]
struct CommitCase {
    id: String,
    chain: usize,
    highest: Option<L>,
    proposed: L,
    committed: bool,
    after: Option<L>,
    error: Option<String>,
}
struct Chain(BTreeMap<B256, Link>);
impl Headers for Chain {
    fn link(&self, hash: B256) -> Option<Link> {
        self.0.get(&hash).copied()
    }
}

#[test]
fn the_three_chain_commit_equals_geth_commit_blocks() {
    let o: CommitOut = read("commit.json");
    let mut bad = vec![];
    let mut committed = 0;
    for c in &o.cases {
        let chain = Chain(
            o.chains[c.chain]
                .iter()
                .map(|l| (l.hash, l.link()))
                .collect(),
        );
        let highest = c.highest.as_ref().map(L::link);
        let got =
            commit::commit_blocks(&chain, o.switch_block, highest.as_ref(), &c.proposed.link());
        let ok = match (&got, c.error.is_some(), c.committed) {
            (Err(_), true, _) => true,
            (Ok(None), false, false) => true,
            (Ok(Some(g)), false, true) => {
                committed += 1;
                c.after
                    .as_ref()
                    .is_some_and(|a| a.hash == g.hash && a.number == g.number && a.round == g.round)
            }
            _ => false,
        };
        if !ok {
            bad.push(c.id.clone());
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {}: {:?}",
        bad.len(),
        o.cases.len(),
        &bad[..bad.len().min(8)]
    );
    assert!(committed > 100);
    println!("commit vectors: {} ({committed} commit)", o.cases.len());
}

/// A sibling of the same height does not shadow the block a hash names: the walk follows parent hashes.
#[test]
fn the_commit_walk_follows_hashes_across_a_side_branch() {
    let h = |n: u8| keccak256([n]);
    let l = |number, round, hash, parent| Link {
        number,
        round,
        hash,
        parent,
    };
    // main: 10 (r8) <- 11 (r9) <- 12 (r10); side: 11' (r5) at the same height as 11
    let chain = Chain(
        [
            l(10, 8, h(10), h(9)),
            l(11, 9, h(11), h(10)),
            l(11, 5, h(111), h(10)),
            l(12, 10, h(12), h(11)),
            l(12, 6, h(112), h(111)),
        ]
        .into_iter()
        .map(|x| (x.hash, x))
        .collect(),
    );
    let main = commit::commit_blocks(&chain, 3, None, &l(12, 10, h(12), h(11))).unwrap();
    assert_eq!(main.map(|x| x.hash), Some(h(10)));
    let side = commit::commit_blocks(&chain, 3, None, &l(12, 6, h(112), h(111))).unwrap();
    assert_eq!(side, None);
}

// ---------------------------------------------------------------- mid-epoch masternode addition (xOneGo #675)

#[derive(Deserialize)]
struct MidOut {
    cfg: CfgIn,
    words_before: Vec<(B256, B256)>,
    words_after: Vec<(B256, B256)>,
    list_epoch_k: Vec<Address>,
    list_epoch_k1: Vec<Address>,
    pool_epoch_k1: Vec<Address>,
    cases: Vec<MidCase>,
}
#[derive(Deserialize)]
struct MidCase {
    round: u64,
    parent_round: u64,
    parent_number: u64,
    epoch_switch: bool,
    leader: Address,
    list_size: usize,
}

#[test]
fn leader_selection_stays_in_phase_across_a_mid_epoch_addition() {
    let o: MidOut = read("midadd.json");
    let spec = spec_of(&o.cfg);
    let v2 = spec.xdpos.v2.as_ref().unwrap();
    let k = gap_snapshot::build(&Words::of(&o.words_before), 3150).unwrap();
    let k1 = gap_snapshot::build(&Words::of(&o.words_after), 4050).unwrap();
    assert_eq!(k.candidates, o.list_epoch_k);
    assert_eq!(k1.candidates, o.pool_epoch_k1);
    assert_eq!((k.candidates.len(), k1.candidates.len()), (8, 9));
    let list9 = masternodes::calc(
        &k1.candidates,
        &[],
        masternodes::max_masternodes(v2, 3600).unwrap(),
    );
    assert_eq!(list9, o.list_epoch_k1);
    let mut bad = vec![];
    for c in &o.cases {
        let (sw, _) = epoch_switch::is_epoch_switch_at_round(
            v2,
            900,
            c.round,
            c.parent_number_for_switch(v2),
            c.parent_round,
        )
        .unwrap();
        // the list of the block: recomputed at a switch, else the list of the parent's epoch
        let list = if sw || c.parent_round >= 3600 {
            &list9
        } else {
            &k.candidates
        };
        let leader = masternodes::leader(c.round, 900, list).unwrap();
        if sw != c.epoch_switch || leader != c.leader || list.len() != c.list_size {
            bad.push(format!("round {} parent {}", c.round, c.parent_round));
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
    // the switch changes the list size inside the epoch's round window: both sizes appear
    assert!(o.cases.iter().any(|c| c.list_size == 8) && o.cases.iter().any(|c| c.list_size == 9));
}

impl MidCase {
    /// The oracle's parent block numbers are never the V2 switch block.
    fn parent_number_for_switch(&self, _: &V2) -> u64 {
        self.parent_number
    }
}

// ---------------------------------------------------------------- the archives: gap snapshots and leaders

#[derive(Deserialize)]
struct GapOut {
    switches: Vec<GapRec>,
    bootstrap: Boot,
}
#[derive(Deserialize)]
struct GapRec {
    id: String,
    number: u64,
    round: u64,
    gap: u64,
    words: Vec<(B256, B256)>,
    validators: Vec<Address>,
    penalties: Vec<Address>,
}
#[derive(Deserialize)]
struct Boot {
    number: u64,
    round: u64,
    switch_extra: Bytes,
    validators: Vec<Address>,
    penalties: Vec<Address>,
}

fn net_spec(net: &str) -> ChainSpec {
    match net {
        "apothem" => apothem(),
        _ => {
            let p = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../conformance/vectors/consensus/net2040.forks.json"
            );
            load_named("net2040", &std::fs::read_to_string(p).unwrap()).unwrap()
        }
    }
}

/// The penalty vector of `conformance/vectors/penalty/` (XB-10) for the same switch, as a chain reader.
#[derive(Deserialize)]
struct PenVec {
    number: u64,
    prev: u64,
    round: u64,
    addrs: Vec<Address>,
    parent_masternodes: Vec<usize>,
    history: Vec<Vec<usize>>,
    miners: Vec<usize>,
    hash15: BTreeMap<String, B256>,
    signs: Vec<(u64, usize, u64)>,
}
struct PenChain {
    headers: BTreeMap<B256, HeaderView>,
    bodies: BTreeMap<B256, Vec<SignTx>>,
    history: Vec<Vec<Address>>,
}
impl BlockReader for PenChain {
    fn header(&self, h: B256, _: u64) -> Option<HeaderView> {
        self.headers.get(&h).copied()
    }
    fn sign_txs(&self, h: B256, _: u64) -> Option<Vec<SignTx>> {
        self.bodies.get(&h).cloned()
    }
}
impl PenaltyHistory for PenChain {
    fn penalties(&self, _: B256, back: usize) -> Option<Vec<Address>> {
        self.history.get(back).cloned()
    }
}
fn pen_chain(v: &PenVec) -> (PenChain, B256) {
    let hash = |n: u64| {
        v.hash15
            .get(&n.to_string())
            .copied()
            .unwrap_or_else(|| keccak256(n.to_be_bytes()))
    };
    let pick = |ix: &[usize]| ix.iter().map(|&i| v.addrs[i]).collect::<Vec<_>>();
    let mut c = PenChain {
        headers: BTreeMap::new(),
        bodies: BTreeMap::new(),
        history: v.history.iter().map(|l| pick(l)).collect(),
    };
    for n in v.prev..v.number {
        let h = HeaderView {
            number: n,
            hash: hash(n),
            parent_hash: hash(n.saturating_sub(1)),
            miner: if n == v.prev {
                Address::ZERO
            } else {
                v.addrs[v.miners[usize::try_from(n - v.prev - 1).unwrap()]]
            },
            epoch_switch: n == v.prev,
        };
        c.headers.insert(h.hash, h);
        c.bodies.insert(h.hash, Vec::new());
    }
    for &(blk, who, signed) in &v.signs {
        c.bodies.get_mut(&hash(blk)).unwrap().push(SignTx {
            sender: v.addrs[who],
            signed: hash(signed),
            ok: true,
        });
    }
    (c, hash(v.number - 1))
}

fn check_network(net: &str, min: usize) {
    let o: GapOut = read(&format!("gap-{net}.json"));
    let spec = net_spec(net);
    let pens: Vec<PenVec> = {
        let p = format!(
            "{}/../../conformance/vectors/penalty/{net}-v2.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    };
    let mut bad = vec![];
    let (mut composed, mut penalised, mut v1_epoch) = (0, 0, 0);
    for r in &o.switches {
        let v2 = spec.xdpos.v2.as_ref().unwrap();
        let pool =
            if gap_snapshot::uses_v1_list(r.number, spec.xdpos.epoch, v2.switch_block).unwrap() {
                v1_epoch += 1;
                gap_snapshot::bootstrap(&o.bootstrap.switch_extra, v2.switch_block).unwrap()
            } else {
                gap_snapshot::build(&Words::of(&r.words), r.gap).unwrap()
            };
        let pv = pens.iter().find(|p| p.number == r.number);
        let got = match pv {
            // penalties from the XB-10 engine over the window of the same switch
            Some(p) => {
                composed += 1;
                let (chain, parent) = pen_chain(p);
                let pm: Vec<Address> = p.parent_masternodes.iter().map(|&i| p.addrs[i]).collect();
                let q = SwitchQuery {
                    number: r.number,
                    parent,
                    round: r.round,
                    parent_round: p.round,
                    parent_masternodes: pm,
                };
                epoch_switch::expected(&spec, &chain, &q, &pool)
            }
            // no window in the archive sample: the penalties are the header's, the pool and the cap are derived
            None => {
                let q = SwitchQuery {
                    number: r.number,
                    parent: B256::ZERO,
                    round: r.round,
                    parent_round: 0,
                    parent_masternodes: vec![],
                };
                epoch_switch::lists(&spec, &q, &pool, |_| Ok(r.penalties.clone()))
            }
        };
        match got {
            Ok(e) if e.validators == r.validators && same_set(&e.penalties, &r.penalties) => {
                penalised += usize::from(!r.penalties.is_empty());
            }
            other => bad.push(format!("{}: {other:?}", r.id)),
        }
    }
    // the first V2 block: the V1 list, no penalties
    let b = &o.bootstrap;
    let pool = gap_snapshot::bootstrap(
        &b.switch_extra,
        spec.xdpos.v2.as_ref().unwrap().switch_block,
    )
    .unwrap();
    let q = SwitchQuery {
        number: b.number,
        parent: B256::ZERO,
        round: b.round,
        parent_round: 0,
        parent_masternodes: vec![],
    };
    let e = epoch_switch::lists(&spec, &q, &pool, |_| Ok(vec![])).unwrap();
    if e.validators != b.validators || !e.penalties.is_empty() || !b.penalties.is_empty() {
        bad.push(format!("{net}-{}-bootstrap", b.number));
    }
    assert!(
        bad.is_empty(),
        "{} of {}:\n{}",
        bad.len(),
        o.switches.len(),
        bad.join("\n")
    );
    assert!(o.switches.len() >= min, "{}", o.switches.len());
    println!("gap snapshot {net}: {} switches equal geth ({composed} with the penalty engine, {penalised} with penalties, {v1_epoch} in the first V2 epoch) + 1 bootstrap", o.switches.len());
}

#[test]
fn apothem_gap_snapshots_at_100_switches_equal_the_archive() {
    check_network("apothem", 100);
}

#[test]
fn net2040_gap_snapshots_at_20_switches_equal_the_archive() {
    check_network("net2040", 20);
}

#[derive(Deserialize)]
struct LeaderOut {
    network: String,
    epoch: u64,
    blocks: Vec<LBlock>,
    lists: Vec<LList>,
}
#[derive(Deserialize)]
struct LBlock {
    number: u64,
    round: u64,
    miner: Address,
}
#[derive(Deserialize)]
struct LList {
    from_number: u64,
    masternodes: Vec<Address>,
}

fn check_leaders(net: &str) {
    let o: LeaderOut = read(&format!("leader-{net}.json"));
    assert_eq!(o.network, net);
    assert_eq!(o.blocks.len(), 50);
    for b in &o.blocks {
        let list = &o
            .lists
            .iter()
            .rev()
            .find(|l| l.from_number <= b.number)
            .unwrap()
            .masternodes;
        assert_eq!(
            masternodes::leader(b.round, o.epoch, list).unwrap(),
            b.miner,
            "{net} block {}",
            b.number
        );
    }
    // the window crosses a switch, so both lists are used
    assert!(
        o.blocks.iter().any(|b| b.number < o.lists[1].from_number)
            && o.blocks.iter().any(|b| b.number >= o.lists[1].from_number)
    );
}

#[test]
fn the_leader_of_50_consecutive_blocks_is_the_actual_author_on_apothem() {
    check_leaders("apothem");
}

#[test]
fn the_leader_of_50_consecutive_blocks_is_the_actual_author_on_net2040() {
    check_leaders("net2040");
}
