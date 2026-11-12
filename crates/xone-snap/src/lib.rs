//! snap/1 for xOne (XB-17): the message codec ([`codec`]), range-proof verification over alloy-trie
//! ([`proof`], [`mpt`]), the server over a state snapshot with the ADR-007 Q15 bounds
//! ([`server`]), and the client state-sync driver with healing, resumable progress and the
//! pre-pivot completeness check ([`client`]), and xpre/1, xOne's preimage extension ([`preimage`]). Specification: `spec/snap/`.
//!
//! No I/O, no clock, no global state: the transport and the peer set are behind
//! [`client::PeerSource`], the time is an argument of [`server::Server::handle`]. [`testkit`]
//! has an in-memory peer and a state generator for tests.
#![forbid(unsafe_code)]

pub mod client;
pub mod codec;
pub mod mpt;
pub mod preimage;
pub mod proof;
pub mod server;
pub mod testkit;
