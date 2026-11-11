//! The runnable node: services over `xone-net`, `xone-sync`, `xone-builder` and `xone-rpc`.
pub mod chain;
pub mod core;
pub mod hist;
pub mod net;
pub mod pool;
pub mod producer;
pub mod rpc;
pub mod safety_file;
pub mod store;

use alloy_primitives::Bytes;
use xone_net::eth::Raw;

/// A transaction as the wire carries it in a list (a typed one is an RLP string around its encoding,
/// a legacy one is the list itself) from its consensus encoding.
pub fn to_wire(tx: &Bytes) -> Raw {
    if tx.first().is_some_and(|b| *b >= 0xc0) {
        Raw(tx.clone())
    } else {
        Raw(Bytes::from(alloy_rlp::encode(tx)))
    }
}

/// The consensus encoding of a wire item: a list is a legacy transaction, a string holds a typed one.
pub fn from_wire(r: &Raw) -> Bytes {
    let mut b = &r.0[..];
    match alloy_rlp::Header::decode(&mut b) {
        Ok(h) if !h.list => Bytes::copy_from_slice(b),
        _ => r.0.clone(),
    }
}

/// State checkpoint spacing, as the replay harness's default.
pub const CHECKPOINT_EVERY: u64 = 1000;

/// A node and the handles of its services, for the binary and for tests.
pub struct Built {
    pub node: crate::Node,
    pub net: net::NetHandle,
    pub core: std::sync::Arc<core::Core>,
}

/// The node of the config: net, chain, pool, producer (with `--mine`) and rpc, in that order (stopped in reverse, each with
/// its deadline). `stamp` is the build's one version string (`xone --version`): it is the p2p
/// client name and `web3_clientVersion`. A restart replays the stored chain here, before the
/// services start.
pub fn build(
    cfg: &crate::Config,
    marker: &crate::datadir::Marker,
    spec: xone_chainspec::ChainSpec,
    genesis: &xone_chainspec::Genesis,
    stamp: &str,
) -> Result<Built, String> {
    use std::{sync::Arc, time::Duration};

    // the producer's checks come first: a refused `--mine` opens nothing
    let producer_plan = cfg
        .mining
        .mine
        .then(|| producer::plan(cfg, spec.chain_id))
        .transpose()
        .map_err(|e| e.to_string())?;

    let core = Arc::new(
        core::Core::open(spec, genesis, &cfg.datadir.join("xone"), CHECKPOINT_EVERY)
            .map_err(|e| e.to_string())?,
    );
    if core.genesis.to_string() != marker.genesis_hash {
        return Err("the genesis hash of the node differs from the chain marker".into());
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("xone-rt")
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let key = net::load_or_create_key(&cfg.datadir)?;
    let mut pc = xone_net::peers::Config::new(
        stamp,
        cfg.network_id.unwrap_or(marker.chain_id),
        core.genesis,
    );
    pc.max_peers = cfg.p2p.max_peers as usize;
    pc.shutdown_grace = Duration::from_secs(1);
    {
        let h = core.hist.read();
        pc.head = h.tip().hash;
        pc.td = h.td;
    }
    let (peers, events) = xone_net::peers::Peers::new(pc.clone(), key);
    let net_svc = net::NetService::new(cfg, rt, peers, key, &pc)?;
    let nh = net_svc.handle();
    let rth = nh.runtime();
    let pool = Arc::new(pool::PoolShared::new(core.clone())?);
    let chain_svc =
        chain::ChainService::new(core.clone(), rth.clone(), nh.clone(), pool.clone(), events);
    let progress = chain_svc.progress.clone();
    let pool_svc = pool::PoolService::new(pool.clone(), rth.clone());
    let producer_svc = producer_plan
        .map(|p| {
            producer::ProducerService::new(&p, cfg, core.clone(), pool.clone(), nh.peers.clone())
        })
        .transpose()?;
    let rpc_svc = rpc::RpcService::new(cfg, rth, core.clone(), progress, pool, nh.clone())?;
    let l = &cfg.limits;
    let mut node = crate::Node::new(
        Duration::from_millis(l.service_stop_timeout_ms),
        Duration::from_millis(l.stop_timeout_ms),
    );
    node.add(Box::new(net_svc));
    node.add(Box::new(chain_svc));
    node.add(Box::new(pool_svc));
    if let Some(p) = producer_svc {
        node.add(Box::new(p));
    }
    node.add(Box::new(rpc_svc));
    Ok(Built {
        node,
        net: nh,
        core,
    })
}
