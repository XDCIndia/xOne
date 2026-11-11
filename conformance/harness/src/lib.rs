//! `xone-replay`: sequential replay of Apothem on xOne's own persisted state (XA-05).
//! Test tool, not shipped. The archive is read only: blocks are fetched once into a cache, and the
//! replay itself is offline.
#![forbid(unsafe_code)]

pub mod cache;
pub mod chain;
pub mod diag;
pub mod pivot;
pub mod replay;
