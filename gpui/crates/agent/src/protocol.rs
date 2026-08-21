//! What crosses the thread boundary: plain, `Send`, serde-shaped data.
//!
//! The interpreter thread never holds a gpui handle. It sends one of these,
//! the pump answers with JSON, and that JSON is what the script sees. Anything
//! that cannot survive that round trip does not belong in the API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A script's handle on one control in one frame.
///
/// It carries the identity as well as the index because the index alone is
/// meaningless across a redraw: checking `(role, label)` turns a
/// silently-wrong click into a `NoSuchNode`, and is what `restale: "match"`
/// rematches on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRef {
    pub frame: u64,
    pub id: usize,
    pub role: String,
    pub label: String,
}

/// What to do when a node's frame is no longer current. Never defaults to
/// anything but [`Restale::Error`] — a script that silently retargets is a
/// script whose failures are invisible.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Restale {
    #[default]
    Error,
    /// Re-snapshot and find the node again by `(role, label)`.
    Match,
}

/// Where a drag ends: on another control, or a displacement from where it
/// started.
///
/// A canvas move — a graph node, a clip on a timeline — has no destination
/// control, so node-to-node cannot express it; a delta can. Externally tagged
/// (`{"node": …}` / `{"by": …}`) rather than untagged so that a malformed
/// target names the field serde could not read, instead of collapsing into
/// "did not match any variant".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DragTarget {
    Node(NodeRef),
    By { dx: f32, dy: f32 },
}

/// One unit of work for the app thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Cmd {
    /// Settle the app and return `{frame, nodes}`.
    Snapshot,
    /// Read the frame counter without settling. Used to stamp an exec result.
    CurrentFrame,
    Click {
        node: NodeRef,
        #[serde(default)]
        restale: Restale,
    },
    Drag {
        from: NodeRef,
        to: DragTarget,
        #[serde(default = "default_steps")]
        steps: u32,
        #[serde(default)]
        restale: Restale,
    },
    /// Focus `node` with a click, then type `text` into it.
    Type {
        node: NodeRef,
        text: String,
        #[serde(default)]
        restale: Restale,
    },
    /// A space-separated keystroke sequence, e.g. `"cmd-p escape"`.
    Key { keys: String },
    /// Dispatch a registered gpui action by name to the focused node.
    Action {
        name: String,
        #[serde(default)]
        payload: Option<Value>,
    },
    /// Let time pass: `n` rounds of "wait, then settle and draw".
    Frames {
        #[serde(default = "default_frames")]
        n: u32,
        /// Wall time to wait before each frame. Not decorative — it is the
        /// only way work running on a runtime gpui does not own (Luma's
        /// database calls live on a Tokio runtime of their own) gets a chance
        /// to finish, because `run_until_parked` only knows about gpui's
        /// executors.
        #[serde(default = "default_wait_ms")]
        wait_ms: u64,
    },
    /// Capture the window, or one node's box within it. Pixel mode only.
    Screenshot {
        #[serde(default)]
        node: Option<NodeRef>,
        #[serde(default)]
        restale: Restale,
    },
    /// Tear the app down and build it again from the same factory and seed.
    Reset,
}

fn default_steps() -> u32 {
    8
}

fn default_frames() -> u32 {
    1
}

/// One frame at 60Hz.
fn default_wait_ms() -> u64 {
    16
}
