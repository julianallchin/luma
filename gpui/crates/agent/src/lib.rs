//! An in-process automation harness for the GPUI app, driven by interpreted
//! code.
//!
//! # Why in-process
//!
//! GPUI paints its own widgets. There is no OS control behind a button, so
//! there is no accessibility tree to query and no external driver — AX, UIA,
//! SkyLight — that can see anything but a rectangle of pixels. The only place
//! that knows a button is a button is the element tree, so that is where the
//! instrumentation goes: `luma_ui::node` publishes a frame of nodes during
//! `prepaint`, and this crate turns "the button labelled Back" into a click at
//! a point.
//!
//! # Shape
//!
//! ```text
//!  MCP client ──stdio──▶ mcp ──▶ interp ──Cmd/JSON──▶ pump ──▶ gpui App
//!                       (thread B)                 (main thread)
//! ```
//!
//! Three layers, and the split between them is not negotiable:
//!
//! - [`pump`] owns the whole gpui side and is the only code that touches
//!   `&mut App`. It is a loop over [`protocol::Cmd`]s.
//! - [`interp`] runs QuickJS on a different thread. It holds a channel, never
//!   a gpui handle, so there is no way for a script to re-enter gpui.
//! - [`mcp`] is the stdio transport, and exposes exactly two tools.
//!
//! Only plain `Send` data crosses between them. That is what makes the
//! interpreter safe to run scripts of unknown quality: the worst a script can
//! do is send a command that fails.
//!
//! # The snapshot invariant
//!
//! Node ids are indices into one frame's registration order, so they mean
//! nothing in any other frame. Every snapshot carries its `frame`, every
//! mutating call carries it back, and a mismatch is a hard error — clicking at
//! coordinates from a frame that has since been redrawn is exactly the failure
//! that makes UI automation untrustworthy. `{restale: "match"}` opts into
//! re-finding the node by `(role, label)`; it is never the default.

pub mod error;
pub mod interp;
pub mod mcp;
#[cfg(feature = "pixel")]
mod pixel;
pub mod protocol;
pub mod pump;

use std::time::Duration;

pub use error::HarnessError;
pub use interp::{ExecResult, Interpreter, API_DTS};
pub use pump::{Config, Mode, PumpClient, RootFactory};

/// The two tools, over one interpreter and one app.
///
/// Construct this on the interpreter's thread; the pump is already running
/// somewhere else by the time you have a [`PumpClient`].
pub struct Harness {
    interpreter: Interpreter,
}

impl Harness {
    pub fn new(client: PumpClient) -> Result<Self, HarnessError> {
        Ok(Self {
            interpreter: Interpreter::new(client)?,
        })
    }

    /// Spin up a headless pump on its own thread and attach a harness to it.
    /// This is the shape a test wants: it already owns a thread, and headless
    /// mode does not care which one gpui runs on.
    pub fn headless(config: Config, root: RootFactory) -> Result<Self, HarnessError> {
        Self::new(pump::spawn(config, root))
    }

    pub fn exec(&mut self, code: &str, timeout: Duration) -> ExecResult {
        self.interpreter.exec(code, timeout)
    }

    pub fn reset(&mut self) -> Result<(), HarnessError> {
        self.interpreter.reset()
    }
}
