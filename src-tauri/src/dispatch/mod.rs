//! The command dispatcher seam: Luma's command surface, decoupled from any
//! host runtime.
//!
//! # Interface
//!
//! The whole seam is three ideas:
//!
//! 1. Build an [`AppServices`] — [`AppServices::headless`] for a host that is
//!    not the Tauri app.
//! 2. Call [`dispatch`] with a command name and its JSON arguments.
//! 3. Handle a [`CommandError`].
//!
//! Everything else — the wire decoding, the handler bodies, the generated
//! `#[tauri::command]` wrappers — is implementation. A host that wants events
//! or process control implements [`EventSink`] and [`Host`]; both have
//! do-nothing defaults ([`Events::discard`], [`HostControl::process_exit`]) so
//! a minimal host implements neither.
//!
//! # Implementation
//!
//! The command table below generates two entry points from one declaration:
//! `adapter::<name>`, a `#[tauri::command]` that injects `AppServices` and
//! lowers `CommandError` to the `String` the wire expects; and an arm of
//! [`dispatch`], which decodes arguments from JSON instead. Declaring the wire
//! name, argument names, argument types and return type exactly once is what
//! keeps two hosts from drifting apart.
//!
//! Wire decoding lives in this file rather than its own module because it and
//! [`dispatch`] know the same thing — the wire schema. Splitting them would be
//! decomposition by chronology, not by knowledge.
//!
//! `docs/specs/dispatcher-port-guide.md` has the recipe for putting a command
//! on the seam, the special cases, and the designs that lost.

#![warn(missing_docs)]

mod error;
pub(crate) mod handlers;
mod services;
mod tauri_host;

pub use error::CommandError;
pub use services::{AppServices, EventSink, Events, Host, HostControl};
pub(crate) use tauri_host::{tauri_events, tauri_host};

use serde::de::DeserializeOwned;
use serde_json::Value;

/// Declare a command once; get the Tauri adapter, the JSON dispatch arm, and
/// the name registry from it.
///
/// Each row reads as `<handler module>::<wire name>(<args>) -> <return type>`.
/// The wire name *is* the handler function name, and the argument names are the
/// handler's parameter names, which Tauri renames `snake_case` → `camelCase` on
/// the wire.
///
/// Every handler is `async`, including the ones whose bodies never await —
/// awaiting a synchronous body costs nothing and keeps the table free of
/// special cases.
macro_rules! commands {
    ($( $domain:ident :: $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) -> $ret:ty );* $(;)?) => {
        /// The Tauri adapter. Each wrapper does the two things a handler
        /// cannot: it receives `State<AppServices>`, and it lowers
        /// [`CommandError`] to the `String` the wire carries. Generated, so a
        /// wrapper cannot drift from its handler.
        ///
        /// Results are returned in their concrete type rather than through
        /// `serde_json::Value`, so the desktop path serializes exactly once.
        pub(crate) mod adapter {
            #![allow(clippy::too_many_arguments, missing_docs)]
            use super::*;
            $(
                #[tauri::command]
                pub async fn $name(
                    services: tauri::State<'_, AppServices>,
                    $($arg: $ty,)*
                ) -> Result<$ret, String> {
                    handlers::$domain::$name(&services, $($arg),*)
                        .await
                        .map_err(String::from)
                }
            )*
        }

        /// Run a command by its wire name against `services`.
        ///
        /// `args` is the same JSON object the frontend passes to `invoke`;
        /// arguments are accepted in either their `camelCase` wire spelling or
        /// their `snake_case` Rust spelling.
        ///
        /// # Errors
        ///
        /// [`CommandError::NotFound`] if no command has that name,
        /// [`CommandError::Invalid`] if an argument is missing or undecodable,
        /// otherwise whatever the command itself returned.
        pub async fn dispatch(
            services: &AppServices,
            name: &str,
            args: &Value,
        ) -> Result<Value, CommandError> {
            match name {
                $(
                    stringify!($name) => {
                        $( let $arg: $ty = decode(args, stringify!($arg))?; )*
                        let value = handlers::$domain::$name(services, $($arg),*).await?;
                        serde_json::to_value(value).map_err(|error| {
                            CommandError::Internal(format!(
                                "failed to serialize `{name}` result: {error}"
                            ))
                        })
                    }
                )*
                other => Err(CommandError::NotFound(format!("unknown command `{other}`"))),
            }
        }

        /// Whether [`dispatch`] owns this wire name.
        ///
        /// A host that also implements commands of its own routes on this.
        #[must_use]
        pub fn handles(name: &str) -> bool {
            DISPATCHED.contains(&name)
        }

        const DISPATCHED: &[&str] = &[$(stringify!($name)),*];
    };
}

use crate::models::agent_threads::{
    AgentThread, AgentThreadMessage, AppendAgentThreadMessagesInput, CreateAgentThreadInput,
};
use crate::models::fixtures::PatchedFixture;
use crate::models::node_graph::{Graph, NodeTypeDef, PatternArgDef};
use crate::models::patterns::PatternSummary;
use crate::models::waveforms::TrackWaveform;
use crate::services::graph_documents::GraphEditResult;

commands! {
    node_graph::get_node_types() -> Vec<NodeTypeDef>;

    patterns::list_patterns() -> Vec<PatternSummary>;
    patterns::get_pattern_args(
        id: String,
        venue_id: Option<String>,
        implementation_id: Option<String>,
    ) -> Vec<PatternArgDef>;
    patterns::save_pattern_graph_document(
        id: String,
        implementation_id: String,
        operation_id: String,
        base_revision: String,
        graph: Graph,
    ) -> GraphEditResult;

    agent_threads::agent_thread_create(input: CreateAgentThreadInput) -> AgentThread;
    agent_threads::agent_thread_append_messages(
        thread_id: String,
        input: AppendAgentThreadMessagesInput,
    ) -> Vec<AgentThreadMessage>;
    agent_threads::agent_thread_rename(thread_id: String, title: Option<String>) -> AgentThread;

    fixtures::get_patched_fixtures(venue_id: String) -> Vec<PatchedFixture>;

    waveforms::get_track_waveform(track_id: String) -> TrackWaveform;

    tracks::update_track_metadata(
        track_id: String,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
    ) -> ();

    midi::midi_release_cue(cue_id: String) -> ();

    sync::force_quit() -> ();
}

// -----------------------------------------------------------------------------
// Wire decoding
// -----------------------------------------------------------------------------
//
// Tauri decodes command arguments from the JS object itself, so the desktop
// adapter never comes through here. A JSON host does, and it has to reproduce
// Tauri's decoding exactly or the frontend breaks silently: handler parameters
// are `snake_case` in Rust and `camelCase` on the wire. Both spellings are
// accepted — the camel one because that is what the frontend sends, the snake
// one so a Rust-side caller writing raw frames doesn't have to guess.
//
// An explicit `null` is treated as an absent key, matching Tauri's handling of
// an omitted optional argument.

fn decode<T: DeserializeOwned>(args: &Value, snake_name: &str) -> Result<T, CommandError> {
    match lookup(args, snake_name) {
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            CommandError::Invalid(format!("bad argument `{snake_name}`: {error}"))
        }),
        // `Option<T>` decodes from null; anything else is genuinely missing.
        None => serde_json::from_value(Value::Null).map_err(|_| {
            CommandError::Invalid(format!("missing required argument `{snake_name}`"))
        }),
    }
}

fn lookup<'a>(args: &'a Value, snake_name: &str) -> Option<&'a Value> {
    let camel = to_camel_case(snake_name);
    args.get(&camel)
        .or_else(|| args.get(snake_name))
        .filter(|value| !value.is_null())
}

fn to_camel_case(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper_next = false;
    for c in snake.chars() {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.push(c.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_names_are_unique() {
        let mut seen = DISPATCHED.to_vec();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(
            seen.len(),
            count,
            "duplicate wire name in the command table"
        );
    }

    #[test]
    fn handles_only_registered_names() {
        assert!(handles("get_node_types"));
        assert!(!handles("run_graph"));
    }

    #[test]
    fn accepts_both_argument_spellings() {
        let camel = json!({ "venueId": "v1" });
        let snake = json!({ "venue_id": "v1" });
        assert_eq!(decode::<String>(&camel, "venue_id").unwrap(), "v1");
        assert_eq!(decode::<String>(&snake, "venue_id").unwrap(), "v1");
    }

    #[test]
    fn missing_optional_is_none_missing_required_is_named() {
        let empty = json!({});
        assert_eq!(decode::<Option<String>>(&empty, "venue_id").unwrap(), None);
        let error = decode::<String>(&empty, "venue_id").unwrap_err();
        assert_eq!(error.to_string(), "missing required argument `venue_id`");
        assert_eq!(error.kind(), "invalid");
    }

    #[test]
    fn explicit_null_reads_as_absent() {
        let nulled = json!({ "venueId": Value::Null });
        assert_eq!(decode::<Option<String>>(&nulled, "venue_id").unwrap(), None);
    }

    #[test]
    fn single_word_names_are_unchanged() {
        assert_eq!(
            decode::<String>(&json!({ "id": "p1" }), "id").unwrap(),
            "p1"
        );
    }

    /// The wire contract: an error's text is exactly the message, never a
    /// variant-decorated version of it.
    #[test]
    fn error_display_is_verbatim() {
        let conflict = CommandError::Conflict {
            expected: Some("a".into()),
            found: Some("b".into()),
            message: "heads differ".into(),
        };
        assert_eq!(String::from(conflict), "heads differ");
        assert_eq!(
            String::from(CommandError::from("plain".to_string())),
            "plain"
        );
    }
}
