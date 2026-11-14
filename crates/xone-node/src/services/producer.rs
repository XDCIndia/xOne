//! The `producer` service (XB-23): builds, seals and sends the blocks of one validator, and drives the V2
//! BFT step machine for it. It runs only with `--mine`, an explicit keystore and password file, and never on
//! chain id 50 (mainnet) in this build (a typed refusal, temporary until Gate D). Apothem (51) is allowed, with the ADR-010 quiet window.
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
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
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
    unit::{SafetyUnit, Sealer},
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
        safety::Kind,
    },
    verify::{Body, Context},
};
use xone_db::Durability;
use xone_executor::{execute_unsealed, BlockOutcome, ExecError};
use xone_net::eth::{BftMessage, Block, Message, NewBlock};
use xone_net::{peers::Peers, rlpx::PeerId};
use xone_primitives::{Header, SyncInfo};
use xone_signer::LocalSigner;
use xone_sync::{
    chain::{insert::Outcome, ChainRead},
    store::Store,
};

use super::{
    chain::Progress,
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

/// The miner's static gas ceiling, read on a chain that has a `DynamicGasLimitBlock` row: geth's default
/// `GasCeil` (`miner/miner.go:70`), the value the geth validators of such a chain build with.
/// ponytail: no `--miner.gaslimit` flag; add it when an operator runs another ceiling.
const GAS_CEIL: u64 = gaslimit::GETH_DEFAULT_GAS_CEIL;
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
    if chain_id == 50 {
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
    /// The number of the last head `on_head` saw; `None` until the first one after the start.
    last_signed_head: Option<u64>,
    /// The parent a block was attempted on (V1), or the round and parent (V2).
    attempted: Option<(u64, B256)>,
    fed: HashSet<B256>,
    signed_for: HashSet<B256>,
    timer: Option<(u64, Instant)>,
    mine_period: u64,
    dead: bool,
    /// The votes, timeouts and SyncInfos of the peers (from the chain service), and a small number per peer
    /// for the machine's `ScorePeer`.
    bft_in: Receiver<(PeerId, BftMessage)>,
    tags: HashMap<PeerId, u64>,
    /// The last store durability that was logged.
    dur: Durability,
    /// The round the machine is in and when it entered it; the last round `stuck` spoke of.
    round_start: (u64, Instant),
    stuck_said: u64,
    /// The last round a stale-round sibling was refused for (said once per round).
    stale_said: u64,
    last_skip: Option<std::mem::Discriminant<schedule::Skip>>,
    /// The last round a timeout was said for (geth resends one every timeout period).
    timeout_said: u64,
    /// The chain service's progress, and whether it proved us caught up once since the start.
    progress: Arc<Progress>,
    caught_up: bool,
    /// The block of `pending.rlp` found at the start, until `finish_pending` decides.
    pending: Option<(Header, Vec<Bytes>)>,
    /// How long the last seal took (record write, sync and signature).
    seal_ms: u128,
    /// Peer votes received per round (the last 16 rounds), for the `stuck` line.
    votes_in: BTreeMap<u64, u32>,
    /// ADR-010 fence clock: valid BFT gossip arrived since the last fence tick, and when that tick was.
    heard: bool,
    fence_at: Instant,
}

fn say(m: impl std::fmt::Display) {
    eprintln!("producer: {m}");
}

/// A head of round `round` or above proves a QC or TC of that round or above: a quorum has left `round` and
/// votes only in its own (`engine_v2/vote.go:414-421`), so a sibling of `round` can never be certified and geth
/// keeps its head (`core/blockchain.go:2413-2424`).
pub fn behind_head(tip: &Header, round: u64) -> bool {
    tip.extra_fields_v2().is_ok_and(|f| f.round >= round)
}

/// The sign blocks below head `number` that a tick skipped: geth signs on every chain-head event of an imported
/// block (`miner/xdpos_worker.go:231-246`, `maybeBroadcastImportedSignTx` `:1119`), but a sign block imported with
/// its child within one tick is never the head the producer sees (xOneTS Gate D defect 15, blocks 5280 and 6810).
/// Every sign block after the last head seen, inside geth's freshness window (`isSigningTxStale`,
/// `:1192-1209`), oldest first. The first head after a start (`last` is `None`) owes none: a restart does not
/// sign again what it signed before.
pub fn skipped_sign_blocks(
    spec: &ChainSpec,
    h: &HistInner,
    last: Option<u64>,
    number: u64,
) -> Vec<(u64, B256)> {
    let Some(last) = last else { return vec![] };
    let from = last
        .saturating_add(1)
        .max(number.saturating_sub(timing::SIGN_FRESHNESS))
        .max(1);
    (from..number)
        .filter(|&n| signtx::is_sign_block(spec, n))
        .filter_map(|n| Some((n, h.hash_at(n)?)))
        .collect()
}

/// After a sibling's unwind: the pool reads the new head, then the transactions of every block taken off, oldest
/// block first, go back (geth `core/txpool/legacypool/legacypool.go` `reset`, the `reinject` set). The pool's own
/// rules drop what the new state refuses (a nonce the chain holds again). Returns how many it took.
pub fn reinject(pool: &PoolShared, dropped: &[Stored]) -> usize {
    pool.reset();
    let raws: Vec<&[u8]> = dropped
        .iter()
        .flat_map(|s| s.body.transactions.iter().map(|t| &t[..]))
        .collect();
    pool.add_raws(&raws).iter().filter(|r| r.is_ok()).count()
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
        // is kept until the node is synced (`finish_pending`)
        if let Ok(raw) = std::fs::read(self.dir.join("pending.rlp")) {
            let mut b: &[u8] = &raw;
            if let (Ok(h), Ok(txs)) = (Header::decode(&mut b), Vec::<Bytes>::decode(&mut b)) {
                self.pending = Some((h, txs));
            }
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

    /// The block that was being signed when the process stopped, once the node is synced: sent if its parent
    /// is still the head (the same bytes, signed again: the unit returns the same signature for the same
    /// intent), dropped if the chain went on without it. Sent at once, before the peers were back, it landed
    /// beside a chain the network had already extended (gate D rehearsal, kill -9 in the seal window after an
    /// 80 s restart).
    fn finish_pending(&mut self) {
        if self.pending.is_none() || self.synced().is_err() {
            return;
        }
        let Some((h, txs)) = self.pending.take() else {
            return;
        };
        let (n, head) = {
            let t = self.core.hist.read();
            (t.tip_number(), t.tip().hash)
        };
        if h.parent_hash == head && h.number == n + 1 {
            say(format!(
                "finishing the block {} that was being signed",
                h.number
            ));
            let body = Body {
                transactions: txs,
                uncles: vec![],
                withdrawals: h.withdrawals_hash.map(|_| vec![]),
            };
            if let Err(e) = self.seal_and_send(h, body, false) {
                say(format!("the pending block is dropped: {e}"));
            }
        } else {
            say(format!(
                "the pending block {} is dropped: the chain is at {n} without it",
                h.number
            ));
        }
        let _ = std::fs::remove_file(self.dir.join("pending.rlp"));
    }

    fn tick(&mut self) {
        self.inbound();
        // The fence (ADR-010 section 8) counts seconds of connected time with valid gossip; nothing advanced it,
        // so on Apothem (quiet = 120) a producer never signed. ponytail: no key sighting yet (fence.sighted),
        // add it when the inbound path recovers signers.
        if self.heard && self.fence_at.elapsed() >= Duration::from_secs(1) {
            let was = self.unit.fence_mut().open();
            self.unit.fence_mut().tick();
            if !was && self.unit.fence_mut().open() {
                say("fence open: the quiet window passed, signing starts");
            }
            self.heard = false;
            self.fence_at = Instant::now();
        }
        self.finish_pending();
        // The head as the BFT machine sees it: the history's tip. `Core::head` is the store's head, which a
        // block reaches before the history does (`Core::import` commits, then derives and pushes); a tick in
        // between fed the machine a Proposal for a block it could not read yet, `on_proposal` dropped it, `fed`
        // kept it from being fed again, and the votes that had arrived first never made the QC: the leader of
        // the next round timed out with the votes in its pool (the round-37 misses, gate D rehearsal).
        let (n, head) = {
            let h = self.core.hist.read();
            (h.tip_number(), h.tip().hash)
        };
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
        let last = self.last_signed_head.replace(number);
        if number == 0 {
            return;
        }
        let (miner, header) = {
            let h = self.core.hist.read();
            let t = h.tip();
            (t.miner, t.header.clone())
        };
        if number > self.switch_block() && self.fed.insert(hash) && miner != self.me {
            self.bft_event(Event::Proposal(hash));
        }
        // the pool admits the special transactions of the head's candidates, not only ours
        let set = self.candidates(&self.core.hist.read());
        self.pool.set_authority(Authority::candidates(set));
        let skipped = skipped_sign_blocks(&self.spec, &self.core.hist.read(), last, number);
        if !skipped.is_empty() {
            say(format!(
                "head {number}: also signing {:?}, sign block(s) imported since head {}",
                skipped.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                last.unwrap_or_default()
            ));
        }
        for (n, h) in skipped {
            self.sign_tx_for(n, h);
        }
        self.sign_tx_for(number, hash);
        let _ = header;
    }

    // ---- sign transactions ----

    /// The candidates of the head's epoch: who may send a special transaction (to 0x89 or 0x90) at gas price 0.
    /// geth's pool admits one from a signer of the head (`hasKnownSpecialCalldata`, `legacypool.go:924-939`,
    /// with the signer set the backend keeps for the head, `eth/backend.go:474-485`): V2 the epoch's masternodes and
    /// its candidate pool (standby and penalised included), V1 the masternodes of the snapshot. Before, the pool
    /// admitted only this node's own, so a geth validator's sign transaction never entered an xone block.
    fn candidates(&self, h: &HistInner) -> Vec<Address> {
        let n = h.tip_number();
        if n >= self.switch_block() {
            h.switch_of(n).map_or(vec![], |w| {
                let mut v = h.switch_masternodes(w);
                if let Ok(p) = h.pool(w.number) {
                    v.extend(p.candidates);
                }
                v
            })
        } else {
            h.snaps.get(&n).map_or(vec![], |s| s.get_signers())
        }
    }

    /// The 0x89 transaction for block `number`, every 15th block and every block of this node's, also when
    /// the node is penalised (it is a candidate then, which is the gate).
    fn sign_tx_for(&mut self, number: u64, hash: B256) {
        if !self.signed_for.insert(hash) || self.stopped() {
            return;
        }
        let candidate = self.candidates(&self.core.hist.read()).contains(&self.me);
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
                Ok(h) => {
                    // and to the peers, as geth's pool announces a local transaction (`eth/handler.go`
                    // `txBroadcastLoop`): before, it reached another validator's block only through ours
                    let msg = Message::Transactions(vec![to_wire(&raw)]);
                    for p in self.peers.peer_ids() {
                        let _ = self.peers.send(&p, msg.clone());
                    }
                    say(format!("sign transaction {h} for block {number}"))
                }
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
        if self.synced().is_err() {
            return;
        }
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
                if let Some(s) = h.at(i) {
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
                let pens = h.header_at(key).map_or(vec![], |s| addresses(&s.penalties));
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
        if self.round_start.0 != round {
            self.round_start = (round, Instant::now());
        }
        let qc = self.bft.highest_qc().proposed_block_info.clone();
        let qc_hash = qc.hash;
        if self.attempted == Some((round, qc_hash)) {
            return;
        }
        let found = {
            let h = self.core.hist.read();
            let tip = h.tip();
            match h.by_hash(qc_hash) {
                None => Err(format!("the block of the highest QC (number {}, round {}) is not here", qc.number, qc.round)),
                // ponytail: a block on the head only
                Some(p) if p.hash != tip.hash => Err(format!(
                    "the highest QC is for block {} round {}, not the head {} (no QC for the head yet)",
                    qc.number, qc.round, tip.header.number
                )),
                Some(p) => {
                    let parent = p.header.clone();
                    match self.v2_header(&parent, round, None, now()) {
                        Err(e) => Err(format!("prepare: {e}")),
                        Ok(skel) => match h.context(&skel, &parent, None) {
                            Ok(Context::V2(c)) => {
                                let v2 = self.spec.xdpos.v2.as_ref();
                                let is_switch = v2.is_some_and(|v| {
                                    epoch_switch::of_header(v, self.spec.xdpos.epoch, &skel)
                                        .is_ok_and(|(s, _)| s)
                                });
                                Ok((parent, c, is_switch))
                            }
                            Ok(Context::V1(_)) => Err("a V1 context above the switch".into()),
                            Err(e) => Err(format!("context: {e}")),
                        },
                    }
                }
            }
        };
        let (parent, ctx, switch) = match found {
            Ok(f) => f,
            Err(why) => {
                if self.sibling(round, &qc) {
                    return;
                }
                return self.stuck(round, &why);
            }
        };
        if let Err(why) = self.synced() {
            return self.stuck(round, why);
        }
        let now_s = now();
        let mp = self.mine_period.max(1);
        if !timing::v2_period_elapsed(parent.timestamp, mp, now_s) {
            return self.stuck(
                round,
                &format!(
                    "mine period {mp} s after parent time {} not over at {now_s}",
                    parent.timestamp
                ),
            );
        }
        match schedule::v2_turn(&self.unit, round, self.spec.xdpos.epoch, &ctx.masternodes) {
            Ok(()) => self.last_skip = None,
            Err(skip) => {
                self.attempted = Some((round, qc_hash));
                // said once per streak of one kind (a penalised node is out of the list for a whole epoch)
                let kind = std::mem::discriminant(&skip);
                if !matches!(skip, schedule::Skip::NotLeader { .. }) && self.last_skip != Some(kind)
                {
                    say(format!("round {round} not proposed: {skip:?}"));
                }
                self.last_skip = Some(kind);
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

    /// geth's worker does not mint before the node is synced (`miner/xdpos_worker.go:383-387`). Here: a peer is
    /// connected, the chain service has seen a peer serve nothing above our head at least once since the start
    /// (its caught-up proof), and no peer announced a block above our head. A block sealed without that reaches
    /// no one, or lands beside a chain the network has already extended.
    fn synced(&mut self) -> Result<(), &'static str> {
        if self.peers.peer_ids().is_empty() {
            return Err("not synced: no peers");
        }
        self.caught_up |= self.progress.confirmed.load(Ordering::Relaxed);
        if !self.caught_up {
            return Err("not synced: no peer has confirmed our head since the start");
        }
        if self.progress.highest.load(Ordering::Relaxed) > self.core.head().0 {
            return Err("not synced: a peer announced a block above our head");
        }
        Ok(())
    }

    /// The leader's sibling (geth `FindParentBlockToAssign`, `engine_v2/engine.go:3294-3319`): when the head
    /// has no QC and the highest QC is for its parent, a leader builds on the QC block, beside the head. geth
    /// keeps its head and inserts the new block on the side (`keepsV2Head`, `core/blockchain.go:2419-2442`); this
    /// node executes on its head only, so it takes the uncertified head off (`Core::unwind_to`, never below the
    /// committed block) and builds on the QC block on the next tick; the dropped block comes back by the branch
    /// rule if the network extends it. Only when this node leads `round`, is synced, has not proposed the round,
    /// and the mine period after the QC block has passed, and only when the head's round is below `round`
    /// ([`behind_head`]). The dropped blocks' transactions go back to the pool ([`reinject`]). True when it
    /// unwound.
    fn sibling(&mut self, round: u64, qc: &xone_primitives::BlockInfo) -> bool {
        if self.synced().is_err() || self.unit.signed_up_to(Kind::Propose) >= round {
            return false;
        }
        let (leads, depth_one, qc_time, stale) = {
            let h = self.core.hist.read();
            let tip = h.tip();
            let Some(p) = h.by_hash(qc.hash) else {
                return false;
            };
            let leads = HistChain(&h).epoch_of_round(round).is_ok_and(|e| {
                let n = e.masternodes.len() as u64;
                n > 0
                    && usize::try_from(round % self.spec.xdpos.epoch % n)
                        .ok()
                        .and_then(|i| e.masternodes.get(i))
                        == Some(&self.me)
            });
            (
                leads,
                tip.header.parent_hash == p.hash,
                p.header.timestamp,
                behind_head(&tip.header, round),
            )
        };
        if !leads
            || !depth_one
            || !timing::v2_period_elapsed(qc_time, self.mine_period.max(1), now())
        {
            return false;
        }
        if stale {
            if self.stale_said != round {
                self.stale_said = round;
                say(format!(
                    "round {round}: no sibling: the head's round is not below ours, a quorum has left this round"
                ));
            }
            return false;
        }
        match self.core.unwind_to(qc.number) {
            Ok(dropped) => {
                let back = reinject(&self.pool, &dropped);
                say(format!(
                    "round {round}: the head has no QC; {} block(s) taken off to build on the QC block {} (the leader's sibling); {back} transaction(s) back in the pool",
                    dropped.len(),
                    qc.number
                ));
                self.last_head = B256::ZERO;
                true
            }
            Err(e) => {
                say(format!("round {round}: no sibling: {e}"));
                false
            }
        }
    }

    /// Why this tick did not reach the turn check, said once per round when the round is older than a mine
    /// period and two seconds (before that, waiting for the QC of the head or the period is normal).
    fn stuck(&mut self, round: u64, why: &str) {
        let late = self.round_start.1.elapsed() > Duration::from_secs(self.mine_period.max(1) + 2);
        if late && self.stuck_said != round {
            self.stuck_said = round;
            let votes = self
                .votes_in
                .get(&round.saturating_sub(1))
                .copied()
                .unwrap_or(0);
            say(format!(
                "round {round}, head {}: {why}; {votes} peer votes received for round {}",
                self.core.head().0,
                round.saturating_sub(1)
            ));
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
                // geth's worker gives an Amsterdam block an empty withdrawals list (`xdpos_worker.go:699-702`)
                withdrawals: self.spec.amsterdam.is_active(number).then(Vec::new),
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
    /// [`Worker::seal_now`], timed: the time is the safety record's write and sync plus the signature, and
    /// goes into the "sealed and sent" line, so a stalled disk shows as a number.
    fn seal(&mut self, h: Header) -> Result<Header, String> {
        let t = Instant::now();
        let r = self.seal_now(h);
        self.seal_ms = t.elapsed().as_millis();
        r
    }

    fn seal_now(&mut self, h: Header) -> Result<Header, String> {
        self.durability();
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
                .then(|| hist.header_at(cp).map(std::borrow::Cow::into_owned))
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
        let t = Instant::now();
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
                withdrawals: body
                    .withdrawals
                    .map(|w| w.into_iter().map(xone_net::eth::Raw).collect()),
            },
            td,
        }));
        for p in self.peers.peer_ids() {
            let _ = self.peers.send(&p, msg.clone());
        }
        say(format!(
            "block {number} {hash} sealed and sent (seal {} ms, import {} ms)",
            self.seal_ms,
            t.elapsed().as_millis()
        ));
        self.sign_tx_for(number, hash);
        Ok(())
    }

    // ---- the BFT machine ----

    /// The durability of the block store (`xone_db::Durability`, ADR-010 section 9), handed to the unit
    /// and the machine before every signature: only `Durable` signs. A producer's store commits each block
    /// to the device (no group commit, `services::build`), so anything else is a fault and is logged.
    fn durability(&mut self) -> Durability {
        let d = self.core.chain.with_store(|s| s.durability());
        if d != self.dur {
            say(format!("store durability {d:?}"));
            self.dur = d;
        }
        self.unit.set_durability(d);
        d
    }

    /// The BFT messages of the peers, in arrival order: verified and handled by the machine, then relayed
    /// to the other peers when the machine found nothing wrong with them (geth `Bfter.Vote`, `Timeout`,
    /// `SyncInfo`: verify, `tryForward`, handle; `eth/bft/handler.go:187-229`).
    fn inbound(&mut self) {
        while let Ok((from, msg)) = self.bft_in.try_recv() {
            if self.stopped() || !self.bft.initialized() {
                continue; // before the V2 switch, or not seeded yet: geth's engine is not ready either
            }
            let next = u64::try_from(self.tags.len()).unwrap_or(u64::MAX);
            let tag = *self.tags.entry(from).or_insert(next);
            let ev = match &msg {
                BftMessage::Vote(v) => {
                    let r = v.proposed_block_info.round;
                    *self.votes_in.entry(r).or_default() += 1;
                    while self.votes_in.len() > 16 {
                        self.votes_in.pop_first();
                    }
                    Event::Vote(v.clone(), Some(tag))
                }
                BftMessage::Timeout(t) => Event::Timeout(t.clone(), Some(tag)),
                BftMessage::SyncInfo(s) => Event::SyncInfo(s.clone(), Some(tag)),
            };
            if self.bft_event(ev) {
                self.heard = true;
                self.peers.relay_bft(&msg, &from);
            }
        }
    }

    /// One event and everything it causes. False when the machine scored a peer for it.
    fn bft_event(&mut self, ev: Event) -> bool {
        let mut clean = true;
        let mut q = VecDeque::from([ev]);
        while let Some(ev) = q.pop_front() {
            if self.stopped() {
                return clean;
            }
            let dur = self.durability();
            let fx = {
                let h = self.core.hist.read();
                self.bft.step(&HistChain(&h), dur, ev)
            };
            let t = Instant::now();
            let driven = drive(&self.bft, &mut self.unit, fx);
            if t.elapsed() > Duration::from_millis(500) {
                say(format!(
                    "slow safety write: a BFT step took {} ms to persist and sign",
                    t.elapsed().as_millis()
                ));
            }
            let d = match driven {
                Ok(d) => d,
                // the quiet window (ADR-010) refuses every signature for a while after a start: that is a
                // skip, not a fault; stopping here left a fresh Apothem producer dead from its first vote
                Err(xone_consensus::unit::UnitError::FenceClosed) => continue,
                Err(e) => {
                    say(format!("stopping: {e}"));
                    self.dead = true;
                    return clean;
                }
            };
            if !d.scores.is_empty() {
                clean = false;
            }
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
                if self.timeout_said != t.round {
                    self.timeout_said = t.round;
                    let votes = self.votes_in.get(&t.round).copied().unwrap_or(0);
                    let head = self.core.head().0;
                    // why the votes made no QC: what the machine holds for the round against the head's epoch
                    let why = {
                        let h = self.core.hist.read();
                        let c = HistChain(&h);
                        let held: Vec<String> = self
                            .bft
                            .votes_at(t.round)
                            .map(|(a, v)| {
                                format!(
                                    "{}:{}@{}/gap{}",
                                    &a.to_string()[..6],
                                    v.proposed_block_info.number,
                                    &v.proposed_block_info.hash.to_string()[..8],
                                    v.gap_number
                                )
                            })
                            .collect();
                        let epoch = c.epoch(&h.tip().hash).map_or_else(
                            |e| format!("epoch of the head: {e}"),
                            |e| {
                                format!(
                                    "head {} epoch gap {} with {} masternodes, candidates {}",
                                    &h.tip().hash.to_string()[..8],
                                    e.gap,
                                    e.masternodes.len(),
                                    c.candidates(e.gap).map_or(0, |x| x.len())
                                )
                            },
                        );
                        format!("held [{}]; {epoch}", held.join(" "))
                    };
                    say(format!("timeout round {} detail: {why}", t.round));
                    say(format!(
                        "timeout for round {} sent (head {head}, {votes} peer votes received for round {}, highest QC round {})",
                        t.round,
                        t.round,
                        self.bft.highest_qc().proposed_block_info.round
                    ));
                }
                self.peers.broadcast_bft(&BftMessage::Timeout(t.clone()));
                q.push_back(Event::Timeout(t, None));
            }
            for s in d.sync_infos {
                self.peers.broadcast_bft(&BftMessage::SyncInfo(s));
            }
        }
        clean
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
        bft_in: Receiver<(PeerId, BftMessage)>,
        progress: Arc<Progress>,
    ) -> Result<Self, String> {
        let signer = xone_signer::local_keystore::load(&plan.keystore, &plan.password, None)
            .map_err(|e| format!("keystore: {e}"))?;
        let me = signer.address();
        let dir = cfg.datadir.join("xone").join("producer");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let store = FileSafety::open(&cfg.datadir.join("xone").join("safety"))
            .map_err(|e| format!("safety store: {e}"))?;
        // ponytail: a private chain has no second signer by construction, so the quiet window is 0; a
        // public chain (Apothem) takes the ADR-010 window W = 120 s (mainnet is refused in `plan`).
        let quiet = if core.spec.chain_id == 51 { 120 } else { 0 };
        let unit = SafetyUnit::open_or_init(store, signer, quiet)
            .map_err(|e| format!("safety unit: {e}"))?;
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
        let dur = core.chain.with_store(|s| s.durability());
        say(format!(
            "validator {me}, safety record at round {}, store durability {dur:?}",
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
                last_signed_head: None,
                attempted: None,
                fed: HashSet::new(),
                signed_for: HashSet::new(),
                timer: None,
                mine_period: 2,
                dead: false,
                bft_in,
                tags: HashMap::new(),
                dur,
                round_start: (0, Instant::now()),
                stuck_said: 0,
                stale_said: 0,
                last_skip: None,
                timeout_said: 0,
                progress,
                caught_up: false,
                pending: None,
                seal_ms: 0,
                votes_in: BTreeMap::new(),
                heard: false,
                fence_at: Instant::now(),
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
