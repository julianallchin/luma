//! Domain behavior the command handlers call.
//!
//! One module per domain, and each one is `pub(crate)` unless something outside
//! the crate genuinely consumes it: the offline binaries (`run_goldens`,
//! `bench_waveform`, `agent_harness`) and the GPUI host reach past
//! [`crate::dispatch`] for bootstrap and for the types dispatched commands
//! return. Those return types (`GraphDocument`, `GraphEditResult`,
//! `TrackEditResult`) belong in [`crate::models`] — until they move, the seam's
//! wire vocabulary is published out of an implementation module, which is why
//! `graph_documents` and `track_edits` are still `pub`.
//!
//! `groups` is `pub` for the same reason: [`crate::stage_render::highlight_state`]
//! already takes its `ResolvedFixture` in a public signature, and the GPUI
//! fixture picker reads an expression back through its parser
//! ([`groups::or_terms`]) rather than growing a second one.

pub(crate) mod agent_execution;
pub(crate) mod authored_documents;
pub(crate) mod authored_merge;
pub(crate) mod authored_state;
pub(crate) mod authored_sync_merge;
pub mod fixtures;
pub mod graph_documents;
pub mod group_derivation;
pub mod groups;
pub mod patch;
pub(crate) mod score_dsl;
pub(crate) mod score_mutations;
pub mod track_edits;
pub mod tracks;
pub mod waveforms;
