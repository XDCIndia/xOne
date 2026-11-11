//! Bootstrap from a snapshot: the authenticated anchor ([`anchor`]), how far back every rule
//! reads ([`retention`]), and what must be on disk before the first block after the pivot can be
//! verified ([`plan`]). Pure code: no I/O, no clock (the caller passes `now`), no global state.
//! Rules, sources and the arithmetic: `spec/retention.md`, `spec/bootstrap-anchor.md`.

pub mod anchor;
pub mod plan;
pub mod retention;

pub use anchor::{verify_anchor, Anchor, AnchorError, Context, Provenance, FORMAT_VERSION};
pub use plan::{pivot_plan, plan_at, Need, Plan};
pub use retention::{Class, Error, Missing, Retention, Rule};
