//! Domain behavior the command handlers call.
//!
//! One module per domain, and each one is `pub(crate)` unless something outside
//! the crate genuinely consumes it: the headless binaries (`luma-cli rpc`,
//! `luma-cli record`, `luma-cli render`) and the GPUI host reach past
//! [`crate::dispatch`] for bootstrap and for the types dispatched commands
//! return. Those return types (`GraphDocument`, `GraphEditResult`,
//! `TrackEditResult`) belong in [`crate::models`] — until they move, the seam's
//! wire vocabulary is published out of an implementation module, which is why
//! `graph_documents` and `track_edits` are still `pub`.
//!
//! `groups` is `pub` for the same reason: the GPUI fixture picker reads an
//! expression back through its parser ([`groups::or_terms`]) rather than
//! growing a second one.
//!
//! `venue_sharing` is `pub` so the UI harness can stand its
//! [`venue_sharing::LocalShareCodes`] in for the server.

pub(crate) mod agent_execution;
pub mod artwork;
pub mod catalog;
pub mod composable_patterns;
pub mod distribute;
pub(crate) mod drafts;
pub mod fixture_create;
pub mod fixtures;
pub mod graph_scores;
pub mod group_derivation;
pub mod groups;
pub mod patch;
pub(crate) mod perform_set;
pub mod stage_ops;
pub mod tracks;
pub mod venue_sharing;
pub mod waveforms;
