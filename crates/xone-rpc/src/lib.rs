//! JSON-RPC for xOne (XB-22). Specification: `spec/rpc/`.
//!
//! [`server`] is the JSON-RPC 2.0 core: request and batch parsing, the case-sensitive method
//! registry, ADR-007 quotas and the error shapes of geth `rpc/errors.go`. The handlers
//! ([`eth`], [`net`], [`web3`], [`txpool`], [`admin`], [`debug`], [`xdpos`], [`xdc_ext`]) run over the
//! traits of [`api`], so the node plugs the chain, state, pool, peers and consensus in later.
//! [`transport`] serves HTTP, WebSocket and IPC; [`subscriptions`] is the bounded fan-out.
//! There is no process-global state: everything lives in an [`server::Rpc`] value.
#![forbid(unsafe_code)]

pub mod admin;
pub mod api;
pub mod arity;
pub mod debug;
pub mod eth;
pub mod json;
pub mod net;
pub mod policy;
pub mod server;
pub mod subscriptions;
pub mod transport;
pub mod txpool;
pub mod types;
pub mod web3;
pub mod xdc;
pub mod xdc_ext;
pub mod xdpos;
