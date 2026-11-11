//! Block building: the transaction pool (XB-16). The block packer (XB-23) reads [`txpool::Pool::pending`].
//!
//! Pure state machines over plain values: no sockets, no clock, no threads and no global state. The caller
//! passes the head state (`xone_state::StateRead`) and the time of the call.
#![forbid(unsafe_code)]

pub mod txpool;
