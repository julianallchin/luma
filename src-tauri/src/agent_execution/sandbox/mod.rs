//! Platform sandbox selection.
//!
//! Today there is exactly one launcher — the developer passthrough. The real
//! ones (macOS Seatbelt via `sandbox-exec`, Linux Landlock + seccomp, design
//! §17.4/§17.5) plug in here and nowhere else: everything above the launcher
//! trait is platform-agnostic.

use crate::agent_execution::worker_launcher::{PassthroughLauncher, WorkerLauncher};

/// The launcher this platform+build should use for agent Python.
///
/// Returning `Err` is a hard stop for the Python tool — never a warn-and-
/// continue (design §17.7).
pub fn default_launcher() -> Result<Box<dyn WorkerLauncher>, String> {
    Ok(Box::new(PassthroughLauncher::new()?))
}
