//! The runnable node: services over `xone-net`, `xone-sync`, `xone-builder` and `xone-rpc`.
pub mod chain;
pub mod core;
pub mod hist;
pub mod net;
pub mod pool;
pub mod producer;
pub mod rpc;
pub mod safety_file;
pub mod stats;
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
        core::Core::open_with(
            spec,
            genesis,
            &cfg.datadir.join("xone"),
            CHECKPOINT_EVERY,
            cfg.sync.minimal,
            // a node that signs commits every block to the device before it goes on
            producer_plan.is_none(),
        )
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
    // the producer's BFT machine reads the votes, timeouts and SyncInfos the follower receives
    let (bft_tx, bft_rx) = std::sync::mpsc::channel();
    let chain_svc = chain::ChainService::new(
        core.clone(),
        rth.clone(),
        nh.clone(),
        pool.clone(),
        events,
        producer_plan.is_some().then_some(bft_tx),
    );
    let progress = chain_svc.progress.clone();
    let pool_svc = pool::PoolService::new(pool.clone(), rth.clone());
    let producer_svc = producer_plan
        .map(|p| {
            producer::ProducerService::new(
                &p,
                cfg,
                core.clone(),
                pool.clone(),
                nh.peers.clone(),
                bft_rx,
            )
        })
        .transpose()?;
    let stats = Arc::new(stats::NodeStats {
        stamp: stamp.to_owned(),
        core: core.clone(),
        progress: progress.clone(),
        pool: pool.clone(),
        peers: nh.peers.clone(),
    });
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
    if cfg.metrics.enabled {
        node.add(Box::new(crate::observe::metrics::MetricsServer::new(
            crate::observe::metrics::MetricsCfg {
                addr: cfg.metrics.addr.clone(),
                port: cfg.metrics.port,
                max_head_age_secs: 60,
            },
            stats.clone(),
        )));
    }
    if let Some(url) = &cfg.ethstats {
        node.add(Box::new(crate::observe::ethstats::Ethstats::new(
            crate::observe::ethstats::EthstatsCfg {
                url: url.clone(),
                allow_plaintext: false,
                identity: identity(cfg, marker, stamp),
            },
            stats.clone(),
            stats,
        )));
    }
    Ok(Built {
        node,
        net: nh,
        core,
    })
}

/// The hello frame's static facts. `node` and `client` are the build's one stamp.
fn identity(
    cfg: &crate::Config,
    marker: &crate::datadir::Marker,
    stamp: &str,
) -> crate::observe::ethstats::Identity {
    crate::observe::ethstats::Identity {
        node: stamp.to_owned(),
        client: stamp.to_owned(),
        port: cfg.p2p.port,
        net: cfg.network_id.unwrap_or(marker.chain_id).to_string(),
        protocol: "eth/63, xdpos/100".into(),
        os: std::env::consts::OS.into(),
        os_v: std::env::consts::ARCH.into(),
        sync_mode: "full".into(),
        gc_mode: cfg.sync.gcmode.clone(),
        state_scheme: "hash".into(),
    }
}
