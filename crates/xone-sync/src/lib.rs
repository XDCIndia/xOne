//! Sync: what a node must hold, and be able to prove, before it follows the chain from a snapshot.
//! This slice is [`bootstrap`]; the wire and the importer are other items.
#![forbid(unsafe_code)]

pub mod bootstrap;
