//! The Tauri commands not yet on the dispatch seam.
//!
//! What is left here is the spawned-progress import path — `import_tracks`,
//! `reprocess_track`, `rekordbox_import_tracks`, `engine_dj_import_tracks` —
//! which threads an `AppHandle` down through `services::tracks` and
//! `preprocessing`, plus `agent_execution`. See
//! `docs/specs/dispatcher-port-guide.md`.

pub mod agent_execution;
pub mod engine_dj;
pub mod rekordbox;
pub mod tracks;
