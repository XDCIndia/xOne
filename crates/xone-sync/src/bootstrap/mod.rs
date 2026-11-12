//! Bootstrap from a snapshot: the authenticated anchor ([`anchor`]), how far back every rule
//! reads ([`retention`]), and what must be on disk before the first block after the pivot can be
//! verified ([`plan`]); then the snapshot itself: its container ([`format`]) and rows
//! ([`manifest`]), the writer ([`export`]), the checked reader ([`import`]) and the minimal mode
//! that keeps only the window ([`minimal`]). No clock (the caller passes `now`), no global state;
//! the only I/O is the reader, writer and staging area a caller passes in.
//! Rules, sources and the arithmetic: `spec/retention.md`, `spec/bootstrap-anchor.md`,
//! `spec/bootstrap/`.

pub mod anchor;
pub mod export;
pub mod format;
pub mod import;
pub mod manifest;
pub mod minimal;
pub mod plan;
pub mod retention;

pub use anchor::{verify_anchor, Anchor, AnchorError, Context, Provenance, FORMAT_VERSION};
pub use plan::{pivot_plan, plan_at, Need, Plan};
pub use retention::{Class, Error, Missing, Retention, Rule};
