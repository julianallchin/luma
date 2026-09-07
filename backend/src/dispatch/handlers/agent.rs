//! Turns, for a host that cannot hold a `TurnStream`.
//!
//! Only three commands are on the seam, and only because a webview cannot own
//! a Rust stream: it starts a turn by name, cancels it by name, and receives
//! deltas as `"agent-turn"` events. A typed host calls
//! [`luma_lib::agent::AgentService::turn`](crate::agent::AgentService::turn)
//! and needs none of this. Thread CRUD stays where it already is, in
//! `handlers::agent_threads`.

use crate::agent::UserPrompt;
use crate::dispatch::{AppServices, CommandError};

/// Start a turn and stream it onto the event bus. Returns the turn id every
/// emitted event carries, so a caller can tell its own turn from another's.
pub async fn agent_turn_start(
    services: &AppServices,
    thread_id: String,
    prompt: String,
) -> Result<String, CommandError> {
    Ok(services
        .agent_turns()
        .start(&thread_id, UserPrompt { text: prompt })?)
}

/// Cancel the thread's running turn, including any Python cell in flight.
/// `false` when there was nothing to cancel.
pub async fn agent_turn_cancel(
    services: &AppServices,
    thread_id: String,
) -> Result<bool, CommandError> {
    Ok(services.agent_turns().cancel(&thread_id))
}

/// Redirect a running turn. Applied at the next assistant-row boundary.
pub async fn agent_steer(
    services: &AppServices,
    thread_id: String,
    message: String,
) -> Result<(), CommandError> {
    services.agent_turns().steer(&thread_id, message)?;
    Ok(())
}
