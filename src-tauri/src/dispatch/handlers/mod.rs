//! Command behavior, free of any host runtime.
//!
//! One module per domain, mirroring `src/commands/`. A handler is a plain
//! `async fn(&AppServices, args…) -> Result<T, CommandError>`: no `State<T>`,
//! no `AppHandle`, no elided lifetime. Every host reaches these same bodies
//! through the generated layers in the parent module, so a command has exactly
//! one implementation.
//!
//! A handler whose body is a single delegating call is still a layer, not a
//! pass-through: it is where service injection happens, and the uniform
//! `(&AppServices, args…)` shape is what lets the command table generate both
//! entry points without per-command special cases.

pub mod agent;
pub mod agent_execution;
pub mod agent_threads;
pub mod annotation_preview;
pub mod artnet;
pub mod auth;
pub mod authored_state;
pub mod categories;
pub mod cloud_sync;
pub mod compositor;
pub mod controller;
pub mod engine_dj;
pub mod fixtures;
pub mod groups;
pub mod host_audio;
pub mod midi;
pub mod mixer;
pub mod node_graph;
pub mod patterns;
pub mod perform;
pub mod rekordbox;
pub mod render_engine;
pub mod score_dsl;
pub mod scores;
pub mod settings;
pub mod skills;
pub mod stage;
pub mod sync;
pub mod telemetry;
pub mod tracks;
pub mod venues;
pub mod waveforms;
