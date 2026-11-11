//! Block building: the transaction pool (XB-16) and the producer (XB-23: schedule, construction, seals, sign
//! transactions, gas limit, slot timing), which reads [`txpool::Pool::pending`].
//!
//! Pure state machines over plain values: no sockets, no clock, no threads and no global state. The caller
//! passes the head state (`xone_state::StateRead`) and the time of the call.
#![forbid(unsafe_code)]

pub mod producer;
pub mod txpool;
