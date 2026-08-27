//! Platform sandbox selection.
//!
//! This is the only place that knows which launcher a platform gets. macOS gets
//! the Seatbelt profile in [`macos`]; other platforms still fall back to the
//! developer passthrough (Linux Landlock is design §17.5, not yet built, and
//! §17.6 says the Python tool does not ship on Windows).

#[cfg(target_os = "macos")]
pub mod macos;

use crate::agent_execution::worker_launcher::{PassthroughLauncher, WorkerLauncher};

/// The one spelling of the escape hatch that skips the sandbox. This module
/// owns the policy, so every launcher reads the name from here rather than
/// repeating the literal. Debug builds honour it freely; §17.7 makes a release
/// build without a real sandbox a hard stop that only this variable lifts.
pub const UNSANDBOXED_VAR: &str = "LUMA_UNSANDBOXED_PYTHON";

/// The launcher this platform+build should use for agent Python.
///
/// Returning `Err` is a hard stop for the Python tool — never a warn-and-
/// continue, and never a silent downgrade to the passthrough (design §17.7).
pub fn default_launcher() -> Result<Box<dyn WorkerLauncher>, String> {
    #[cfg(target_os = "macos")]
    {
        if cfg!(debug_assertions) && std::env::var(UNSANDBOXED_VAR).as_deref() == Ok("1") {
            log::warn!(
                "[agent-exec] {UNSANDBOXED_VAR}=1: running agent Python WITHOUT a sandbox. \
                 Agent code can read your home directory, your Luma databases and any \
                 credential this process can see. Debug builds only."
            );
            return Ok(Box::new(PassthroughLauncher::new()?));
        }
        Ok(Box::new(macos::SeatbeltLauncher::new()?))
    }
    #[cfg(not(target_os = "macos"))]
    Ok(Box::new(PassthroughLauncher::new()?))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn macos_gets_the_seatbelt_launcher_by_default() {
        // If the override were set, every sandboxed acceptance test in this
        // crate would silently be testing nothing.
        assert!(std::env::var(UNSANDBOXED_VAR).is_err());
        assert_eq!(default_launcher().unwrap().name(), "seatbelt");
    }
}
