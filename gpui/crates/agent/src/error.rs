//! Every way a harness call can fail, in one enum.
//!
//! These cross a thread boundary and end up as a JavaScript exception, so they
//! are plain data with a stable `Display` — the message is the contract, not
//! the variant.

use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    /// A mutating call carried a frame that is no longer current. The snapshot
    /// the script is holding describes a UI that has since been redrawn, so
    /// acting on it would click whatever happens to be at those coordinates
    /// now. See the snapshot invariant in the crate docs.
    StaleFrame { snapshot: u64, current: u64 },
    /// The node reference did not match anything in the current frame — either
    /// the id is out of range, or the (role, label) at that id is not the one
    /// the script thinks it is.
    NoSuchNode { role: String, label: String },
    /// The node exists but is entirely clipped away, so there is no point on
    /// screen that would hit it.
    NotVisible { role: String, label: String },
    /// The pump did not answer within the deadline. The app thread is wedged;
    /// it cannot be unwedged, so every later call will fail the same way until
    /// the server is restarted.
    Timeout { waited: Duration },
    /// The pump thread is gone (it panicked, or the harness was dropped).
    PumpGone,
    /// `App::build_action` rejected the name or the payload.
    BadAction(String),
    /// The call needs a capability this mode does not have — a screenshot in
    /// headless mode, say.
    Unsupported(&'static str),
    /// A malformed call from the JS side: unknown command, bad argument shape.
    BadCall(String),
}

impl fmt::Display for HarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Spelled exactly as the harness contract requires.
            Self::StaleFrame { snapshot, current } => {
                write!(
                    f,
                    "StaleFrame: snapshot was {snapshot}, current is {current}"
                )
            }
            Self::NoSuchNode { role, label } => {
                write!(f, "NoSuchNode: no {role} labelled {label:?} in this frame")
            }
            Self::NotVisible { role, label } => {
                write!(f, "NotVisible: {role} {label:?} is clipped out of view")
            }
            Self::Timeout { waited } => {
                write!(f, "Timeout: the app did not respond within {waited:?}")
            }
            Self::PumpGone => write!(f, "PumpGone: the app thread is no longer running"),
            Self::BadAction(message) => write!(f, "BadAction: {message}"),
            Self::Unsupported(what) => write!(f, "Unsupported: {what}"),
            Self::BadCall(message) => write!(f, "BadCall: {message}"),
        }
    }
}

impl std::error::Error for HarnessError {}
