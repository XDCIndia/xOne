//! The `producer` service (XB-23): builds, seals and sends the blocks of one validator, and drives the V2
//! BFT step machine for it. It runs only with `--mine`, an explicit keystore and password file, and never on
//! chain ids 50 and 51 in this build (a typed refusal, temporary until Gate D).
//!
//! One std thread polls the chain head every 50 ms. Everything that signs goes through the safety unit
//! (`xone_consensus::unit`): the validator key is moved into it at start. The pure rules are in
//! `xone_builder::producer`; this file reads the chain and the clock, executes the transactions
//! (`xone_executor::execute_unsealed`), imports the sealed block through `Core::import` (the full header check
//! and execution again) and sends it.
//!
//! Crash safety: the block to be signed is written to `producer/pending.rlp` before the unit signs it, so a
//! restart that finds the signature in the record signs the same bytes again and sends the same block (the
//! signer is deterministic). The highest QC and TC are written to `producer/syncinfo.rlp` as the machine asks
//! (`Effect::Store`) and fed back at start.
//!
//! ponytail: the producer builds on the head of the chain only (the node executes no side branches yet), the
//! M2 co-signature of another masternode's V1 block and the V1 checkpoint M2 tokens of more than one
//! masternode (Go's `math/rand` source) are not wired, and execution is not interruptible inside one block
//! (a stop is honoured between transactions, between phases and while waiting for a slot).

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use alloy_primitives::{Address, Bytes, B256};
use alloy_rlp::{Decodable, Encodable};
use xone_builder::{
    producer::{
        build::{assemble, select, Commit, Params, Roots},
        gaslimit, schedule, seal_v1, seal_v2, signtx, timing,
    },
    txpool::{special::Authority, PoolTx},
};
use xone_chainspec::ChainSpec;
use xone_consensus::{
    unit::SafetyUnit,
    v1::{
        checkpoint::addresses,
        penalty::{penalties_v1, penalty_era, scan_blocks, PenaltyEra},
        turn,
    },
    v2::{
        self,
        bft::{drive::drive, Bft, Chain, Config as BftConfig, EpochFacts, Event, HeaderFacts},
        cert::Secp,
        epoch_switch, gap_snapshot,
    },
    verify::{Body, Context},
};
use xone_db::Durability;
use xone_executor::{execute_unsealed, BlockOutcome, ExecError};
use xone_net::eth::{BftMessage, Block, Message, NewBlock};
use xone_net::peers::Peers;
use xone_primitives::{Header, SyncInfo};
use xone_signer::LocalSigner;
use xone_sync::chain::{insert::Outcome, ChainRead};

use super::{
    core::{now, Core, Dyn},
    hist::{HistInner, Stored},
    pool::PoolShared,
    safety_file::FileSafety,
    to_wire,
};
use crate::{
    lifecycle::{Service, ServiceError},
    Config,
};

/// The miner's static gas ceiling, read on a chain that has a `DynamicGasLimitBlock` row (geth `GasCeil`
/// is an operator flag; here it is the 420M plateau of the XDC networks).
const GAS_CEIL: u64 = gaslimit::XDC_BLOCK_GAS_LIMIT;
const TICK: Duration = Duration::from_millis(50);

/// Why `--mine` is refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProducerError {
    /// Temporary: public producers wait for Gate D (an isolated, soaked producer) and the owner's mint test.
    #[error("--mine is refused on chain id {0} in this build: block production on a public network waits for Gate D")]
    PublicNetwork(u64),
    #[error("--mine needs --keystore: no key is picked up from the datadir")]
    NoKeystore,
    #[error("--mine needs --password, a file that holds the keystore password")]
    NoPassword,
}

/// The files of a producer, after the checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub keystore: PathBuf,
    pub password: PathBuf,
}

/// The checks of `--mine`, in order: the network, then the two files that must be named.
pub fn plan(cfg: &Config, chain_id: u64) -> Result<Plan, ProducerError> {
    if matches!(chain_id, 50 | 51) {
        return Err(ProducerError::PublicNetwork(chain_id));
    }
    let keystore = cfg
        .mining
        .keystore
        .clone()
        .ok_or(ProducerError::NoKeystore)?;
    let password = cfg
        .mining
        .password_file
        .clone()
        .ok_or(ProducerError::NoPassword)?;
    Ok(Plan { keystore, password })
}

type Unit = SafetyUnit<FileSafety, LocalSigner>;

// ---- the chain, as the BFT machine reads it ----

fn facts(s: &Stored) -> HeaderFacts {
    let f = s.header.extra_fields_v2().ok();
    HeaderFacts {
        number: s.header.number,
        hash: s.hash,
        parent: s.header.parent_hash,
        round: f.as_ref().map_or(0, |f| f.round),
        qc: f.map(|f| f.quorum_cert),
    }
}

struct HistChain<'a>(&'a HistInner);

impl HistChain<'_> {
    fn epoch_of(&self, number: u64) -> Result<Arc<EpochFacts>, v2::Error> {
        let spec = &self.0.spec;
        let w = self.0.switch_of(number).ok_or(v2::Error::Missing {
            what: "epoch switch",
            number,
        })?;
        Ok(Arc::new(EpochFacts {
            masternodes: self.0.switch_masternodes(w),
            gap: gap_snapshot::gap_number(w.number, spec.xdpos.epoch, spec.xdpos.gap)?,
            switch: w.hash,
        }))
    }
}

impl Chain for HistChain<'_> {
    fn header(&self, hash: &B256) -> Option<Arc<HeaderFacts>> {
        self.0.by_hash(*hash).map(|s| Arc::new(facts(s)))
    }
    fn head(&self) -> Option<Arc<HeaderFacts>> {
        Some(Arc::new(facts(self.0.tip())))
    }
    fn epoch(&self, hash: &B256) -> Result<Arc<EpochFacts>, v2::Error> {
        let s = self
            .0
            .by_hash(*hash)
            .ok_or(v2::Error::MissingHeader { hash: *hash })?;
        self.epoch_of(s.header.number)
    }
    // ponytail: a round of an epoch that has no switch block yet answers with the last known epoch.
    fn epoch_of_round(&self, round: u64) -> Result<Arc<EpochFacts>, v2::Error> {
        let w = self
            .0
            .switches
            .iter()
            .rev()
            .find(|w| w.round <= round)
            .or(self.0.switches.last())
            .ok_or(v2::Error::Missing {
                what: "epoch switch",
                number: round,
            })?;
        self.epoch_of(w.number)
    }
    fn candidates(&self, gap: u64) -> Option<Arc<[Address]>> {
        let spec = &self.0.spec;
        let w = self.0.switches.iter().rev().find(|w| {
            gap_snapshot::gap_number(w.number, spec.xdpos.epoch, spec.xdpos.gap).ok() == Some(gap)
        })?;
        self.0.pool(w.number).ok().map(|s| s.candidates.into())
    }
    fn timeout_gap(&self, round: u64) -> Result<u64, v2::Error> {
        self.epoch_of_round(round).map(|e| e.gap)
    }
}

// ---- the worker ----

struct Worker {
    core: Arc<Core>,
    pool: Arc<PoolShared>,
    peers: Peers,
    unit: Unit,
    bft: Bft<Secp>,
    me: Address,
    spec: ChainSpec,
    stop: Arc<AtomicBool>,
    dir: PathBuf,
    last_head: B256,
    /// The parent a block was attempted on (V1), or the round and parent (V2).
    attempted: Option<(u64, B256)>,
    fed: HashSet<B256>,
    signed_for: HashSet<B256>,
    timer: Option<(u64, Instant)>,
    mine_period: u64,
    dead: bool,
}

fn say(m: impl std::fmt::Display) {
    eprintln!("producer: {m}");
}

/// Writes `bytes` to `path` and syncs it, through a temporary file and a rename.
fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    if let Some(d) = path.parent() {
        std::fs::File::open(d)
            .and_then(|d| d.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl Worker {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.dead
    }

    /// Waits `d`, or less when a stop is requested.
    fn wait(&self, d: Duration) {
        let end = Instant::now() + d;
        while !self.stopped() && Instant::now() < end {
            thread::sleep(TICK.min(end.saturating_duration_since(Instant::now())));
        }
    }

    fn switch_block(&self) -> u64 {
        self.spec
            .xdpos
            .v2
            .as_ref()
            .map_or(u64::MAX, |v| v.switch_block)
    }

    // ---- start ----

    fn recover(&mut self) {
        // a block that was about to be signed when the process stopped
        let p = self.dir.join("pending.rlp");
        if let Ok(raw) = std::fs::read(&p) {
            let mut b: &[u8] = &raw;
            if let (Ok(h), Ok(txs)) = (Header::decode(&mut b), Vec::<Bytes>::decode(&mut b)) {
                let (n, head) = self.core.head();
                if h.parent_hash == head && h.number == n + 1 {
                    say(format!(
                        "finishing the block {} that was being signed",
                        h.number
                    ));
                    let body = Body {
                        transactions: txs,
                        uncles: vec![],
                        withdrawals: None,
                    };
                    if let Err(e) = self.seal_and_send(h, body, false) {
                        say(format!("the pending block is dropped: {e}"));
                    }
                }
            }
            let _ = std::fs::remove_file(&p);
        }
        // the highest QC and TC the machine asked to keep
        if let Ok(raw) = std::fs::read(self.dir.join("syncinfo.rlp")) {
            if let Ok(si) = SyncInfo::decode(&mut &raw[..]) {
                if self.core.head().0 >= self.switch_block() {
                    self.bft_event(Event::Synced);
                    self.bft_event(Event::SyncInfo(si, None));
                }
            }
        }
    }

    // ---- the loop ----

    fn run(mut self) {
        self.recover();
        while !self.stopped() {
            self.tick();
            self.wait(TICK);
        }
        say("stopped");
    }

    fn tick(&mut self) {
        let (n, head) = self.core.head();
        if head != self.last_head {
            self.last_head = head;
            self.on_head(n, head);
        }
        if n >= self.switch_block() {
            self.v2_tick(n);
        } else {
            self.v1_tick(n);
        }
    }

    /// A new head: the sign transaction of a block that is not ours, and the proposal for the BFT machine.
    fn on_head(&mut self, number: u64, hash: B256) {
        if number == 0 {
            return;
        }
        let (miner, header) = {
            let h = self.core.hist.read();
            let t = h.tip();
            (t.miner, t.header.clone())
        };
        self.sign_tx_for(number, hash);
        if number > self.switch_block() && self.fed.insert(hash) && miner != self.me {
            self.bft_event(Event::Proposal(hash));
        }
        let _ = header;
    }

    // ---- sign transactions ----

    fn is_candidate(&self, h: &HistInner) -> bool {
        let n = h.tip_number();
        if n >= self.switch_block() {
            h.switch_of(n).is_some_and(|w| {
                h.switch_masternodes(w).contains(&self.me)
                    || h.pool(w.number)
                        .is_ok_and(|p| p.candidates.contains(&self.me))
            })
        } else {
            h.snaps
                .get(&n)
                .is_some_and(|s| s.get_signers().contains(&self.me))
        }
    }

    /// The 0x89 transaction for block `number`, every 15th block and every block of this node's, also when
    /// the node is penalised (it is a candidate then, which is the gate).
    fn sign_tx_for(&mut self, number: u64, hash: B256) {
        if !self.signed_for.insert(hash) || self.stopped() {
            return;
        }
        let candidate = self.is_candidate(&self.core.hist.read());
        let state_nonce = {
            let (_, head) = self.core.head();
            let mut out = None;
            let _ = self.core.chain.reader().with_state(head, &mut |s| {
                out = Some(xone_state::StateRead::account(&Dyn(s), self.me).map_or(0, |a| a.nonce));
            });
            out
        };
        let pool_nonce = xone_rpc::api::PoolApi::pending_nonce(&*self.pool, self.me).unwrap_or(0);
        let head = self.core.head().0;
        match signtx::emit(
            &self.spec,
            number,
            hash,
            head,
            candidate,
            pool_nonce,
            state_nonce,
            &mut self.unit,
        ) {
            Ok(Some(raw)) => match self.pool.add_raws(&[&raw]).remove(0) {
                Ok(h) => say(format!("sign transaction {h} for block {number}")),
                Err(e) => say(format!(
                    "sign transaction for block {number} not added: {e}"
                )),
            },
            Ok(None) => {}
            Err(e) => say(format!("sign transaction for block {number}: {e}")),
        }
    }

    // ---- V1 ----

    fn v1_tick(&mut self, n: u64) {
        let (head, parent_creator, ctx, snap_signers, parent) = {
            let h = self.core.hist.read();
            let tip = h.tip();
            if self.attempted == Some((n, tip.hash)) {
                return;
            }
            let Some(snap) = h.snaps.get(&n) else { return };
            // the header is a skeleton: the context only reads its number and the chain
            let skel = Header {
                number: n + 1,
                parent_hash: tip.hash,
                extra_data: vec![0; 97].into(),
                ..Header::default()
            };
            let c = match h.context(&skel, &tip.header, Some(snap)) {
                Ok(Context::V1(c)) => c,
                Ok(Context::V2(_)) => return,
                Err(e) => return say(format!("no V1 context for block {}: {e}", n + 1)),
            };
            (
                tip.hash,
                tip.miner,
                *c,
                snap.get_signers(),
                tip.header.clone(),
            )
        };
        let epoch = self.spec.xdpos.epoch;
        let me = self.me;
        let now_s = now();
        let slot = match schedule::v1_slot(
            &ctx.parent_masternodes,
            n,
            (n > 0).then_some(parent_creator),
            me,
            epoch,
        ) {
            Ok(s) => s,
            Err(e) => return say(format!("no V1 slot: {e}")),
        };
        if !schedule::v1_may_seal(slot, parent.timestamp, now_s)
            || !timing::v1_parent_second_passed(parent.timestamp, now_s)
        {
            return;
        }
        self.attempted = Some((n, head));
        match self.v1_build(
            &parent,
            parent_creator,
            &ctx.parent_masternodes,
            &snap_signers,
            slot,
        ) {
            Ok(()) => {}
            Err(e) => say(format!("block {} not produced: {e}", n + 1)),
        }
    }

    fn v1_build(
        &mut self,
        parent: &Header,
        parent_creator: Address,
        list: &[Address],
        signers: &[Address],
        slot: schedule::V1Slot,
    ) -> Result<(), String> {
        let number = parent.number + 1;
        let epoch = self.spec.xdpos.epoch;
        let difficulty = turn::difficulty(
            list,
            parent.number,
            (parent.number > 0).then_some(parent_creator),
            self.me,
        )
        .map_err(|e| e.to_string())?;
        let checkpoint = (number.is_multiple_of(epoch) && number >= epoch)
            .then(|| self.v1_checkpoint(number, list, signers))
            .transpose()?;
        let time = timing::v1_block_time(parent.timestamp, self.spec.xdpos.period, now());
        let h = seal_v1::prepare(&seal_v1::Prepare {
            parent,
            gas_limit: gaslimit::gas_limit(&self.spec, parent.gas_limit, number, GAS_CEIL),
            base_fee: gaslimit::base_fee(&self.spec, number),
            difficulty,
            time,
            checkpoint,
            vanity: &[],
        });
        let masternodes = list.len();
        let (body, out) = self.fill(parent, &h, self.me)?;
        let h = assemble(h, &Roots::from(&out));
        // a difficulty-1 block waits a random part of the wiggle before it is sent (`Seal`)
        let mut j = [0u8; 8];
        getrandom::fill(&mut j).map_err(|e| e.to_string())?;
        let delay = timing::v1_release_delay(
            h.timestamp,
            now_ms(),
            u64::try_from(difficulty).unwrap_or(1),
            masternodes,
            u64::from_le_bytes(j),
        );
        let _ = slot;
        self.write_pending(&h, &body)?;
        self.seal_and_send_after(h, body, delay)
    }

    /// The `Penalties`, `Validators` and masternode list of the V1 checkpoint `number` (geth `Prepare`,
    /// `xdpos.go:1439-1480`).
    fn v1_checkpoint(
        &self,
        number: u64,
        prev: &[Address],
        signers: &[Address],
    ) -> Result<seal_v1::Checkpoint, String> {
        let h = self.core.hist.read();
        let spec = &self.spec;
        let epoch = spec.xdpos.epoch;
        let era = penalty_era(
            spec.effective.tip_signing.is_active(number),
            spec.effective.tip_randomize.is_active(number),
        );
        let tip2019 = match spec.effective.tip2019 {
            xone_chainspec::ForkBlock::At(b) => b,
            _ => u64::MAX,
        };
        let current = if era == PenaltyEra::Creators {
            let mut counts: BTreeMap<Address, u64> = BTreeMap::new();
            for i in scan_blocks(era, number, epoch, tip2019) {
                if let Some(s) = usize::try_from(i).ok().and_then(|i| h.blocks.get(i)) {
                    *counts.entry(s.miner).or_default() += 1;
                }
            }
            penalties_v1(era, prev, &counts).map_err(|e| e.to_string())?
        } else if era == PenaltyEra::ComebackOnly {
            vec![]
        } else {
            return Err("the V1 penalty list before TIPSigning reads the 0x89 state, which the producer does not".into());
        };
        let mut earlier = BTreeMap::new();
        for i in 1..=v1_limit() {
            if number > epoch * i {
                let key = number - epoch * i;
                let pens = usize::try_from(key)
                    .ok()
                    .and_then(|k| h.blocks.get(k))
                    .map_or(vec![], |s| addresses(&s.header.penalties));
                earlier.insert(key, pens);
            }
        }
        let removed =
            xone_consensus::v1::checkpoint::checkpoint_penalties(number, epoch, &current, &earlier)
                .map_err(|e| e.to_string())?;
        let masternodes: Vec<Address> = signers
            .iter()
            .filter(|s| !removed.contains(s))
            .copied()
            .collect();
        if masternodes.is_empty() {
            return Err("every signer is penalised".into());
        }
        // `BuildValidatorFromM2` of `GenM2FromRandomize` with no secrets: one masternode is its own M2.
        // More than one needs Go's `math/rand` source, which is not ported.
        if masternodes.len() != 1 {
            return Err(
                "the M2 assignment of more than one masternode is not wired into the producer"
                    .into(),
            );
        }
        Ok(seal_v1::Checkpoint {
            masternodes,
            penalties: current,
            validators: vec![0, 0, 0, b'0'].into(),
        })
    }

    // ---- V2 ----

    fn v2_tick(&mut self, n: u64) {
        if !self.bft.initialized() {
            self.bft_event(Event::Synced);
            if !self.bft.initialized() {
                return;
            }
        }
        if let Some((token, at)) = self.timer {
            if Instant::now() >= at {
                self.timer = None;
                self.bft_event(Event::Timer(token));
            }
        }
        let round = self.bft.round();
        let qc_hash = self.bft.highest_qc().proposed_block_info.hash;
        if self.attempted == Some((round, qc_hash)) {
            return;
        }
        let (parent, ctx, switch) = {
            let h = self.core.hist.read();
            let Some(p) = h.by_hash(qc_hash) else { return };
            if p.hash != h.tip().hash {
                return; // ponytail: a block on the head only
            }
            let parent = p.header.clone();
            let skel = match self.v2_header(&parent, round, None, now()) {
                Ok(s) => s,
                Err(_) => return,
            };
            let Ok(Context::V2(c)) = h.context(&skel, &parent, None) else {
                return;
            };
            let v2 = self.spec.xdpos.v2.as_ref();
            let is_switch = v2.is_some_and(|v| {
                epoch_switch::of_header(v, self.spec.xdpos.epoch, &skel).is_ok_and(|(s, _)| s)
            });
            (parent, c, is_switch)
        };
        let now_s = now();
        let mp = self.mine_period.max(1);
        if !timing::v2_period_elapsed(parent.timestamp, mp, now_s) {
            return;
        }
        match schedule::v2_turn(&self.unit, round, self.spec.xdpos.epoch, &ctx.masternodes) {
            Ok(()) => {}
            Err(_) => {
                self.attempted = Some((round, qc_hash));
                return;
            }
        }
        self.attempted = Some((round, qc_hash));
        if let Err(e) = self.v2_build(
            &parent,
            round,
            switch.then_some((ctx.masternodes, ctx.penalties)),
            now_s,
        ) {
            say(format!("block {} (round {round}) not produced: {e}", n + 1));
        }
    }

    fn v2_header(
        &self,
        parent: &Header,
        round: u64,
        switch: Option<(Vec<Address>, Vec<Address>)>,
        now_s: u64,
    ) -> Result<Header, seal_v2::Error> {
        let number = parent.number + 1;
        seal_v2::prepare(&seal_v2::Prepare {
            parent,
            round,
            qc: self.bft.highest_qc(),
            me: self.me,
            gas_limit: gaslimit::gas_limit(&self.spec, parent.gas_limit, number, GAS_CEIL),
            base_fee: gaslimit::base_fee(&self.spec, number),
            time: timing::v2_block_time(parent.timestamp, self.mine_period.max(1), now_s),
            switch,
        })
    }

    fn v2_build(
        &mut self,
        parent: &Header,
        round: u64,
        switch: Option<(Vec<Address>, Vec<Address>)>,
        now_s: u64,
    ) -> Result<(), String> {
        let h = self
            .v2_header(parent, round, switch, now_s)
            .map_err(|e| e.to_string())?;
        let (body, out) = self.fill(parent, &h, self.me)?;
        let h = assemble(h, &Roots::from(&out));
        self.write_pending(&h, &body)?;
        self.seal_and_send(h, body, true)
    }

    // ---- block body ----

    /// Selects the transactions and executes them: a transaction the execution refuses is dropped with the
    /// later ones of its sender, and the block is executed again.
    fn fill(
        &mut self,
        parent: &Header,
        h: &Header,
        author: Address,
    ) -> Result<(Body, BlockOutcome), String> {
        let number = h.number;
        let base_fee = h.base_fee.map(|b| u128::try_from(b).unwrap_or(u128::MAX));
        let (pending, raws) = self.pool.pending_raws(base_fee.unwrap_or(0));
        let p = Params {
            number,
            gas_limit: h.gas_limit,
            soft_gas_cap: 0,
            base_fee,
            max_sign_txs: match self.spec.xdpos.max_masternodes_v2 {
                0 => 256,
                m => 2 * usize::try_from(m).unwrap_or(128),
            },
        };
        let stop = self.stop.clone();
        // ponytail: the gas a transaction uses is its declared gas here, so a block may be fuller than it
        // could be, never over the limit; the pool's exact accounting needs per-transaction execution.
        let mut txs: Vec<Arc<PoolTx>> = select(pending, &p, |t| {
            if stop.load(Ordering::Relaxed) {
                Commit::Invalid
            } else {
                Commit::Used(t.tx.gas)
            }
        });
        for _ in 0..8 {
            if self.stopped() {
                return Err("stopped".into());
            }
            let body = Body {
                transactions: txs
                    .iter()
                    .filter_map(|t| raws.get(&t.hash).cloned())
                    .collect(),
                uncles: vec![],
                withdrawals: None,
            };
            let r = {
                let hist = self.core.hist.read();
                let mut out = None;
                self.core
                    .chain
                    .reader()
                    .with_state(parent.hash(), &mut |s| {
                        out = Some(execute_unsealed(
                            &self.spec,
                            &Dyn(s),
                            h,
                            author,
                            &body,
                            parent,
                            &*hist,
                        ));
                    })
                    .map_err(|e| e.to_string())?;
                out.ok_or("no parent state")?
            };
            match r {
                Ok(o) => return Ok((body, o)),
                Err(
                    ExecError::Tx { index, .. }
                    | ExecError::Execution { index, .. }
                    | ExecError::GasLimitReached { index }
                    | ExecError::Special { index, .. },
                ) if index < txs.len() => {
                    let from = txs[index].from();
                    let nonce = txs[index].tx.nonce;
                    say(format!(
                        "dropping a transaction the execution refused: {}",
                        txs[index].hash
                    ));
                    txs.retain(|t| !(t.from() == from && t.tx.nonce >= nonce));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("too many refused transactions".into())
    }

    // ---- seal, import, send ----

    fn write_pending(&self, h: &Header, body: &Body) -> Result<(), String> {
        let mut b = Vec::new();
        h.encode(&mut b);
        body.transactions.encode(&mut b);
        write_durable(&self.dir.join("pending.rlp"), &b)
    }

    fn seal_and_send_after(
        &mut self,
        h: Header,
        body: Body,
        delay: Duration,
    ) -> Result<(), String> {
        let sealed = self.seal(h)?;
        self.wait(delay);
        if self.stopped() {
            return Err("stopped".into());
        }
        self.import_and_send(sealed, body)
    }

    fn seal_and_send(&mut self, h: Header, body: Body, v2_vote: bool) -> Result<(), String> {
        let sealed = self.seal(h)?;
        // a block whose time is still ahead waits for it (geth `commitMinedBlock`, `xdpos_worker.go:957-967`)
        let ahead = sealed.timestamp.saturating_sub(now());
        self.wait(Duration::from_secs(ahead));
        if self.stopped() {
            return Err("stopped".into());
        }
        let hash = sealed.hash();
        self.import_and_send(sealed, body)?;
        if v2_vote {
            self.fed.insert(hash);
            self.bft_event(Event::Proposal(hash));
        }
        Ok(())
    }

    /// The seal: the unit decides, writes its record, then signs (`seal_v1::seal`, `seal_v2::seal`).
    fn seal(&mut self, h: Header) -> Result<Header, String> {
        let spec = &self.spec;
        let number = h.number;
        if number > self.switch_block() {
            let round = h.extra_fields_v2().map_err(|e| e.to_string())?.round;
            return seal_v2::seal(h, round, &mut self.unit).map_err(|e| e.to_string());
        }
        let epoch = spec.xdpos.epoch;
        let tip_randomize = spec.effective.tip_randomize.is_active(number);
        let checkpoint = {
            let hist = self.core.hist.read();
            let cp = number - number % epoch;
            (cp != number && cp != 0)
                .then(|| {
                    usize::try_from(cp)
                        .ok()
                        .and_then(|c| hist.blocks.get(c))
                        .map(|s| s.header.clone())
                })
                .flatten()
        };
        let s = seal_v1::seal(h, &mut self.unit, epoch, checkpoint.as_ref(), tip_randomize)
            .map_err(|e| e.to_string())?;
        if let Some(m2) = s.needs_m2 {
            return Err(format!(
                "the assigned M2 is {m2}: a co-signature of another masternode is not wired"
            ));
        }
        Ok(s.header)
    }

    fn import_and_send(&mut self, h: Header, body: Body) -> Result<(), String> {
        let hash = h.hash();
        let number = h.number;
        let out = self
            .core
            .import(h.clone(), body.clone(), now())
            .map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(self.dir.join("pending.rlp"));
        if out != Outcome::Extended {
            return Err(format!("not adopted: {out:?}"));
        }
        self.last_head = hash;
        let td = self.core.hist.read().td;
        self.peers.set_head(hash, td);
        let msg = Message::NewBlock(Box::new(NewBlock {
            block: Block {
                header: h,
                transactions: body.transactions.iter().map(to_wire).collect(),
                uncles: vec![],
            },
            td,
        }));
        for p in self.peers.peer_ids() {
            let _ = self.peers.send(&p, msg.clone());
        }
        say(format!("block {number} {hash} sealed and sent"));
        self.sign_tx_for(number, hash);
        Ok(())
    }

    // ---- the BFT machine ----

    fn bft_event(&mut self, ev: Event) {
        let mut q = VecDeque::from([ev]);
        while let Some(ev) = q.pop_front() {
            if self.stopped() {
                return;
            }
            let fx = {
                let h = self.core.hist.read();
                self.bft.step(&HistChain(&h), Durability::Durable, ev)
            };
            let d = match drive(&self.bft, &mut self.unit, fx) {
                Ok(d) => d,
                Err(e) => {
                    say(format!("stopping: {e}"));
                    self.dead = true;
                    return;
                }
            };
            for l in &d.commits {
                say(format!("committed block {} round {}", l.number, l.round));
            }
            for s in &d.stores {
                let mut b = Vec::new();
                s.encode(&mut b);
                if let Err(e) = write_durable(&self.dir.join("syncinfo.rlp"), &b) {
                    say(format!("the sync info is not kept: {e}"));
                }
            }
            if let Some((token, after)) = d.timer {
                self.timer = Some((token, Instant::now() + after));
            }
            if let Some((_, mp)) = d.rounds.last() {
                self.mine_period = *mp;
            }
            for (kind, why) in &d.refused {
                say(format!("the safety unit refused a {kind:?}: {why}"));
            }
            for v in d.votes {
                self.peers.broadcast_bft(&BftMessage::Vote(v.clone()));
                q.push_back(Event::Vote(v, None));
            }
            for t in d.timeouts {
                self.peers.broadcast_bft(&BftMessage::Timeout(t.clone()));
                q.push_back(Event::Timeout(t, None));
            }
            for s in d.sync_infos {
                self.peers.broadcast_bft(&BftMessage::SyncInfo(s));
            }
        }
    }
}

fn v1_limit() -> u64 {
    xone_consensus::v1::checkpoint::LIMIT_PENALTY_EPOCH
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

// ---- the service ----

pub struct ProducerService {
    worker: Option<Worker>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ProducerService {
    /// Opens the key and the safety record. The key is moved into the unit and goes nowhere else.
    pub fn new(
        plan: &Plan,
        cfg: &Config,
        core: Arc<Core>,
        pool: Arc<PoolShared>,
        peers: Peers,
    ) -> Result<Self, String> {
        let signer = xone_signer::local_keystore::load(&plan.keystore, &plan.password, None)
            .map_err(|e| format!("keystore: {e}"))?;
        let me = signer.address();
        let dir = cfg.datadir.join("xone").join("producer");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let store = FileSafety::open(&cfg.datadir.join("xone").join("safety"))
            .map_err(|e| format!("safety store: {e}"))?;
        // ponytail: a private chain has no second signer by construction, so the quiet window is 0; a
        // public chain is refused above and takes the ADR-010 window when it is allowed.
        let unit =
            SafetyUnit::open_or_init(store, signer, 0).map_err(|e| format!("safety unit: {e}"))?;
        let spec = core.spec.clone();
        let v2 = spec.xdpos.v2.clone().ok_or("the chain has no V2 section")?;
        let bft = Bft::new(
            BftConfig {
                v2,
                gap: spec.xdpos.gap,
                me: Some(me),
            },
            Secp,
            unit.record().clone(),
        )
        .map_err(|e| e.to_string())?;
        // the producer's own sign transactions are admitted: this node is a candidate
        pool.set_authority(Authority::candidates([me]));
        let stop = Arc::new(AtomicBool::new(false));
        say(format!(
            "validator {me}, safety record at round {}",
            unit.record().signed[2].round
        ));
        Ok(Self {
            worker: Some(Worker {
                core,
                pool,
                peers,
                unit,
                bft,
                me,
                spec,
                stop: stop.clone(),
                dir,
                last_head: B256::ZERO,
                attempted: None,
                fed: HashSet::new(),
                signed_for: HashSet::new(),
                timer: None,
                mine_period: 2,
                dead: false,
            }),
            stop,
            thread: None,
        })
    }
}

impl Service for ProducerService {
    fn name(&self) -> &str {
        "producer"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let w = self
            .worker
            .take()
            .ok_or_else(|| ServiceError("producer restarted".into()))?;
        self.thread = Some(
            thread::Builder::new()
                .name("producer".into())
                .spawn(move || w.run())
                .map_err(|e| ServiceError(e.to_string()))?,
        );
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
