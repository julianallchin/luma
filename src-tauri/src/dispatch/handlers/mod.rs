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

pub mod agent_threads;
pub mod fixtures;
pub mod midi;
pub mod node_graph;
pub mod patterns;
pub mod sync;
pub mod tracks;
pub mod waveforms;
