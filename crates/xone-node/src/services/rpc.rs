//! The `rpc` service: binds the five `xone-rpc` traits to the live chain, state, pool, peers and
//! consensus, and serves HTTP, WebSocket and IPC per the config. Only the namespaces of `--http.api`
//! and `--ws.api` are registered, as geth does.
//!
//! ponytail: `eth_call`, `eth_estimateGas` and the tracers answer a typed refusal (there is no
//! single-call executor yet), `XDPoS_getMissedRoundsInEpochByBlockNum` too. `newHeads` and `logs`
//! are published by `Core` on each canonical block; `newPendingTransactions` accepts and stays quiet.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use alloy_primitives::{Address, Bytes, B256};
use tokio::{net::TcpListener, runtime::Handle, task::JoinHandle};
use xone_consensus::{
    v1::{
        checkpoint::{addresses, masternodes_of},
        snapshot::Snapshot,
    },
    v2::gap_snapshot,
};
use xone_rpc::{
    api::{
        Backend, BlockView, CallOutput, CallRequest, ChainApi, ConsensusApi, EpochInfo,
        MissedRounds, NetApi, NodeInfo, PeerInfo, StateApi, TraceOut, Tracer,
    },
    policy::{registry, Policy},
    server::{Limits, Rpc},
    transport::{bind_ipc, serve_http, serve_ipc_on, serve_ws},
    types::RpcError,
};
use xone_state::StateRead;
use xone_sync::{
    bootstrap::retention::{Class, Missing},
    chain::ChainRead,
};

use super::{chain::Progress, core::Core, net::NetHandle, pool::PoolShared, to_wire};
use crate::{
    config::{Config, Limits as NodeLimits},
    enode::Enode,
    lifecycle::{Service, ServiceError},
};

struct RpcChain {
    core: Arc<Core>,
    progress: Arc<Progress>,
}

impl ChainApi for RpcChain {
    fn spec(&self) -> &xone_chainspec::ChainSpec {
        &self.core.spec
    }
    fn head(&self) -> (u64, B256) {
        self.core.head()
    }
    fn hash_by_number(&self, number: u64) -> Option<B256> {
        self.core.hist.read().hash_at(number)
    }
    fn block(&self, hash: B256) -> Option<BlockView> {
        let header = self.core.hist.read().header_by_hash(hash)?.into_owned();
        let (body, _) = self.core.records(header.number, hash)?;
        let raw = alloy_rlp::encode(xone_net::eth::Block {
            header: header.clone(),
            transactions: body.transactions.iter().map(to_wire).collect(),
            uncles: body.uncles.clone(),
            withdrawals: body
                .withdrawals
                .as_ref()
                .map(|w| w.iter().cloned().map(xone_net::eth::Raw).collect()),
        });
        Some(BlockView {
            hash,
            header,
            txs: body.transactions,
            uncles: body.uncles,
            raw: Bytes::from(raw),
        })
    }
    fn header(&self, hash: B256) -> Option<xone_primitives::Header> {
        Some(self.core.hist.read().header_by_hash(hash)?.into_owned())
    }
    fn receipts(&self, hash: B256) -> Option<Vec<Bytes>> {
        let n = self.core.hist.read().number_of(hash)?;
        Some(self.core.records(n, hash)?.1)
    }
    fn tx_location(&self, tx: B256) -> Option<(B256, u64)> {
        let h = self.core.hist.read();
        let (n, i) = h.tx(tx)?;
        Some((h.hash_at(n)?, u64::from(i)))
    }
    fn history(&self, class: Class, block: u64) -> Result<(), Missing> {
        // A node that followed from genesis holds every block. A minimal node holds what its
        // store holds, and answers for the rest with the typed error, never an empty list.
        self.core
            .held()
            .map_or(Ok(()), |h| h.is_within(class, block))
    }
    fn syncing(&self) -> Option<(u64, u64, u64)> {
        let (head, _) = self.core.head();
        let p = &self.progress;
        let highest = p.highest.load(Ordering::Relaxed);
        (head < highest || !p.confirmed.load(Ordering::Relaxed))
            .then(|| (p.start.load(Ordering::Relaxed), head, highest.max(head)))
    }
    fn mining_as(&self) -> Option<Address> {
        None
    }
    fn finalized(&self) -> Option<u64> {
        self.core.chain.committed().map(|l| l.number)
    }
}

struct RpcState {
    core: Arc<Core>,
}

fn unsupported(what: &str) -> RpcError {
    RpcError::server(format!("{what} is not served yet by this node"))
}

impl StateApi for RpcState {
    fn state(&self, block: B256, f: &mut dyn FnMut(&dyn StateRead)) -> Result<(), Missing> {
        // the chain holds its read lock for the whole of `f`: the state cannot leave the retained
        // window while it is read
        if self.core.chain.reader().with_state(block, f).is_err() {
            let (head, _) = self.core.head();
            let number = self.core.hist.read().number_of(block).unwrap_or(head);
            return Err(Missing::UnsupportedHistory {
                class: Class::State,
                block: number,
                floor: head.saturating_sub(super::core::RETENTION),
                pivot: 0,
            });
        }
        Ok(())
    }
    fn code(&self, code_hash: B256) -> Option<Bytes> {
        self.core.hist.read().codes.get(&code_hash).cloned()
    }
    fn call(&self, _: B256, _: &CallRequest, _: &AtomicBool) -> Result<CallOutput, RpcError> {
        Err(unsupported("eth_call"))
    }
    fn estimate_gas(&self, _: B256, _: &CallRequest, _: &AtomicBool) -> Result<u64, RpcError> {
        Err(unsupported("eth_estimateGas"))
    }
    fn trace(
        &self,
        _: B256,
        _: Option<usize>,
        _: &Tracer,
        _: &AtomicBool,
    ) -> Result<Vec<TraceOut>, RpcError> {
        Err(unsupported("tracing"))
    }
}

struct RpcNet {
    net: NetHandle,
    core: Arc<Core>,
}

impl NetApi for RpcNet {
    fn client_version(&self) -> String {
        self.net.name.clone()
    }
    fn network_id(&self) -> u64 {
        self.net.network_id
    }
    fn peer_count(&self) -> usize {
        self.net.peers.peer_count()
    }
    fn listening(&self) -> bool {
        true
    }
    fn node_info(&self) -> NodeInfo {
        NodeInfo {
            id: alloy_primitives::hex::encode(self.net.id),
            name: self.net.name.clone(),
            enode: self.net.enode.clone(),
            ip: self
                .net
                .listen
                .rsplit_once(':')
                .map_or_else(String::new, |(i, _)| i.to_owned()),
            listen_addr: self.net.listen.clone(),
            network: self.net.network_id,
            genesis: self.net.genesis,
            head: self.core.head().1,
        }
    }
    fn peers(&self) -> Vec<PeerInfo> {
        self.net
            .peers
            .peer_ids()
            .into_iter()
            .map(|id| PeerInfo {
                id: alloy_primitives::hex::encode(id),
                name: String::new(),
                enode: format!("enode://{}", alloy_primitives::hex::encode(id)),
                remote: String::new(),
                inbound: false,
                trusted: false,
                caps: vec![],
            })
            .collect()
    }
    fn add_peer(&self, enode: &str, trusted: bool) -> Result<bool, RpcError> {
        let e: Enode = enode
            .parse()
            .map_err(|e: crate::enode::EnodeError| RpcError::invalid_params(e.to_string()))?;
        self.net.add_peer(&e, trusted).map_err(RpcError::server)?;
        Ok(true)
    }
    fn remove_peer(&self, _: &str, _: bool) -> Result<bool, RpcError> {
        Ok(false)
    }
}

struct RpcConsensus {
    core: Arc<Core>,
}

impl RpcConsensus {
    fn number_of(&self, hash: B256) -> Result<u64, RpcError> {
        self.core
            .hist
            .read()
            .number_of(hash)
            .ok_or_else(|| RpcError::server("unknown block"))
    }
    fn is_v2(&self, n: u64) -> bool {
        self.core
            .spec
            .xdpos
            .v2
            .as_ref()
            .is_some_and(|v| n > v.switch_block)
    }
}

impl ConsensusApi for RpcConsensus {
    fn snapshot(&self, hash: B256) -> Result<Snapshot, RpcError> {
        let n = self.number_of(hash)?;
        if self.is_v2(n) {
            return Err(RpcError::server("not supported in the v2 consensus"));
        }
        self.core
            .hist
            .read()
            .snaps
            .get(&n)
            .cloned()
            .ok_or_else(|| RpcError::server(format!("the snapshot of block {n} is not held")))
    }
    fn masternodes(&self, hash: B256) -> Result<Vec<Address>, RpcError> {
        let n = self.number_of(hash)?;
        let h = self.core.hist.read();
        if self.is_v2(n) {
            let w = h
                .switch_of(n)
                .ok_or_else(|| RpcError::server("no epoch switch"))?;
            return Ok(h.switch_masternodes(w));
        }
        let cp = n - n % self.core.spec.xdpos.epoch;
        let s = h
            .header_at(cp)
            .ok_or_else(|| RpcError::server("no checkpoint header"))?;
        masternodes_of(&s.extra_data).map_err(|e| RpcError::server(e.to_string()))
    }
    fn round(&self, hash: B256) -> Result<u64, RpcError> {
        let h = self.core.hist.read();
        let s = h
            .header_by_hash(hash)
            .ok_or_else(|| RpcError::server("unknown block"))?;
        s.extra_fields_v2()
            .map(|f| f.round)
            .map_err(|e| RpcError::server(e.to_string()))
    }
    fn penalties(&self, hash: B256) -> Result<Vec<Address>, RpcError> {
        let n = self.number_of(hash)?;
        let h = self.core.hist.read();
        let w = h
            .switch_of(n)
            .ok_or_else(|| RpcError::server("no epoch switch"))?;
        Ok(h.switch_pen(w))
    }
    fn standbynodes(&self, hash: B256) -> Result<Vec<Address>, RpcError> {
        let n = self.number_of(hash)?;
        let h = self.core.hist.read();
        let w = h
            .switch_of(n)
            .ok_or_else(|| RpcError::server("no epoch switch"))?;
        let pool = h
            .pool(w.number)
            .map_err(|e| RpcError::server(e.to_string()))?;
        Ok(gap_snapshot::standby(
            &pool.candidates,
            &h.switch_masternodes(w),
            &addresses(
                &h.header_at(w.number)
                    .ok_or_else(|| RpcError::server("no switch header"))?
                    .penalties,
            ),
        ))
    }
    fn epoch_info(&self, epoch: u64) -> Result<EpochInfo, RpcError> {
        let h = self.core.hist.read();
        let i = h
            .switches
            .iter()
            .position(|w| w.epoch == epoch)
            .ok_or_else(|| RpcError::server("epoch not found"))?;
        Ok(EpochInfo {
            hash: h.switches[i].hash,
            round: h.switches[i].round,
            first_block: h.switches[i].number,
            last_block: h.switches.get(i + 1).map(|w| w.number - 1),
        })
    }
    fn epoch_switches_between(&self, begin: B256, end: B256) -> Result<Vec<u64>, RpcError> {
        let (b, e) = (self.number_of(begin)?, self.number_of(end)?);
        Ok(self
            .core
            .hist
            .read()
            .switches
            .iter()
            .filter(|w| (b..=e).contains(&w.number))
            .map(|w| w.number)
            .collect())
    }
    fn missed_rounds(&self, _: B256) -> Result<MissedRounds, RpcError> {
        Err(unsupported("XDPoS_getMissedRoundsInEpochByBlockNum"))
    }
}

fn limits(l: &NodeLimits) -> Limits {
    let d = Limits::default();
    Limits {
        http_connections: l.rpc_http_connections as usize,
        http_waiting: l.rpc_http_waiting as usize,
        http_in_flight: l.rpc_http_in_flight as usize,
        ws_connections: l.rpc_ws_connections as usize,
        ipc_connections: l.rpc_ipc_connections as usize,
        body_bytes: usize::try_from(l.rpc_body_bytes).unwrap_or(d.body_bytes),
        batch_items: l.rpc_batch_items as usize,
        response_bytes: usize::try_from(l.rpc_response_bytes).unwrap_or(d.response_bytes),
        ..d
    }
}

/// An `Rpc` that serves only `namespaces` (geth `--http.api`) and checks `Host` against `vhosts` and
/// `Origin` against `origins`. An unknown namespace is a startup error.
fn rpc_for(
    be: &Backend,
    lim: &Limits,
    namespaces: &[String],
    policy: Policy,
) -> Result<Arc<Rpc>, String> {
    let reg = registry(namespaces).map_err(|e| e.to_string())?;
    let mut rpc = Rpc::new(be.clone(), lim.clone());
    // `Rpc::new` returns the only handle, so the registry can be replaced before it is shared
    if let Some(r) = Arc::get_mut(&mut rpc) {
        r.reg = reg;
        r.policy = policy;
    }
    Ok(rpc)
}

/// `--rpc.jwtsecret`: the file holds 32 bytes in hex, `0x` optional (geth `node.ObtainJWTSecret`).
/// A missing, unreadable or malformed file stops the start; nothing is generated, so a typo never
/// leaves the endpoint open. The secret and the file content never reach an error message.
pub fn load_jwt_secret(path: &Path) -> Result<[u8; 32], String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("--rpc.jwtsecret {}: {}", path.display(), e.kind()))?;
    if meta.len() > 256 {
        return Err(format!(
            "--rpc.jwtsecret {}: not a 32-byte hex secret",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("--rpc.jwtsecret {}: {}", path.display(), e.kind()))?;
    let t = text.trim();
    let t = t.strip_prefix("0x").unwrap_or(t);
    alloy_primitives::hex::decode(t)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .ok_or_else(|| {
            format!(
                "--rpc.jwtsecret {}: not a 32-byte hex secret",
                path.display()
            )
        })
}

pub struct RpcService {
    rt: Handle,
    http: Option<(SocketAddr, Arc<Rpc>)>,
    ws: Option<(SocketAddr, Arc<Rpc>)>,
    ipc: Option<(PathBuf, Arc<Rpc>)>,
    tasks: Vec<JoinHandle<()>>,
}

impl RpcService {
    pub fn new(
        cfg: &Config,
        rt: Handle,
        core: Arc<Core>,
        progress: Arc<Progress>,
        pool: Arc<PoolShared>,
        net: NetHandle,
    ) -> Result<Self, String> {
        let be = Backend {
            chain: Arc::new(RpcChain {
                core: core.clone(),
                progress,
            }),
            state: Arc::new(RpcState { core: core.clone() }),
            pool,
            net: Arc::new(RpcNet {
                net,
                core: core.clone(),
            }),
            consensus: Arc::new(RpcConsensus { core: core.clone() }),
        };
        let lim = limits(&cfg.limits);
        let addr = |a: &str, p: u16| {
            let host = if a == "localhost" { "127.0.0.1" } else { a };
            format!("{host}:{p}")
                .parse::<SocketAddr>()
                .map_err(|e| format!("{a}:{p}: {e}"))
        };
        let http = cfg
            .http
            .enabled
            .then(|| {
                Ok::<_, String>((
                    addr(&cfg.http.addr, cfg.http.port)?,
                    rpc_for(
                        &be,
                        &lim,
                        &cfg.http.api,
                        Policy {
                            vhosts: cfg.http.vhosts.clone(),
                            origins: vec![],
                            jwt: cfg
                                .http
                                .jwt_secret
                                .as_deref()
                                .map(load_jwt_secret)
                                .transpose()?,
                        },
                    )?,
                ))
            })
            .transpose()?;
        let ws = cfg
            .ws
            .enabled
            .then(|| {
                Ok::<_, String>((
                    addr(&cfg.ws.addr, cfg.ws.port)?,
                    rpc_for(
                        &be,
                        &lim,
                        &cfg.ws.api,
                        Policy {
                            vhosts: vec![],
                            origins: cfg.ws.origins.clone(),
                            jwt: cfg
                                .ws
                                .jwt_secret
                                .as_deref()
                                .map(load_jwt_secret)
                                .transpose()?,
                        },
                    )?,
                ))
            })
            .transpose()?;
        let ipc = (!cfg.ipc.disabled)
            .then(|| {
                Ok::<_, String>((
                    cfg.ipc
                        .path
                        .clone()
                        .unwrap_or_else(|| cfg.datadir.join("xone.ipc")),
                    // the IPC endpoint is local (socket mode 0600) and serves every namespace, as geth's
                    rpc_for(
                        &be,
                        &lim,
                        &["eth", "net", "web3", "txpool", "admin", "debug", "XDPoS"]
                            .map(String::from),
                        Policy::default(),
                    )?,
                ))
            })
            .transpose()?;
        // the transports that can push: their hubs receive the canonical blocks
        core.hubs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(
                ws.iter()
                    .map(|(_, r)| r.hub.clone())
                    .chain(ipc.iter().map(|(_, r)| r.hub.clone())),
            );
        Ok(Self {
            rt,
            http,
            ws,
            ipc,
            tasks: vec![],
        })
    }
}

impl Service for RpcService {
    fn name(&self) -> &str {
        "rpc"
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        let err = |e: String| ServiceError(e);
        for (kind, bound) in [("http", self.http.clone()), ("ws", self.ws.clone())] {
            if let Some((addr, rpc)) = bound {
                let l = self
                    .rt
                    .block_on(TcpListener::bind(addr))
                    .map_err(|e| err(format!("{kind} listen {addr}: {e}")))?;
                eprintln!("rpc: {kind} listening on {addr}");
                self.tasks.push(if kind == "http" {
                    self.rt.spawn(serve_http(rpc, l))
                } else {
                    self.rt.spawn(serve_ws(rpc, l))
                });
            }
        }
        if let Some((path, rpc)) = self.ipc.clone() {
            // Bound here, not in the task: an unusable path fails the start, and the listeners
            // already started are stopped, so no partial RPC service reports success.
            let bound = {
                let _rt = self.rt.enter();
                bind_ipc(&path)
            };
            let l = match bound {
                Ok(l) => l,
                Err(e) => {
                    for t in self.tasks.drain(..) {
                        t.abort();
                    }
                    return Err(err(format!("ipc listen {}: {e}", path.display())));
                }
            };
            eprintln!("rpc: ipc listening on {}", path.display());
            self.tasks.push(self.rt.spawn(async move {
                if let Err(e) = serve_ipc_on(rpc, l).await {
                    eprintln!("rpc: ipc stopped: {e}");
                }
            }));
        }
        Ok(())
    }

    fn stop(&mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
        if let Some((path, _)) = &self.ipc {
            let _ = std::fs::remove_file(path);
        }
    }
}
