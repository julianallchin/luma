//! Hydrate a fresh Claude CLI session from Luma's own transcript.
//!
//! When [`state::RunLease::resume`] misses — a different engine or model ran
//! last, the transcript moved, or this machine has simply never run a native
//! session for this thread — the old fallback was `continuation()`: the
//! whole thread reserialized as JSON *text* inside one user prompt, which is
//! how one real conversation opened a 571k-token prompt cache on its first
//! turn (see `turn.rs`'s `continuation` doc comment). A figure landing that
//! way is never seen as a picture either — it is just base64 characters in a
//! string.
//!
//! The right fix is to give the CLI a *real* session to resume, built from
//! rows Luma already has. `SessionStore.load()` in the Agent SDK does the
//! same thing in-process ("materialized to a temporary JSONL file; the
//! subprocess resumes from that file using its existing resume code" — Agent
//! SDK `sdk.d.ts`); Luma drives the bare `claude` CLI, so this does the
//! equivalent by hand: write a session transcript in the exact on-disk shape
//! the CLI's own sessions use, at the exact path `--resume <id>` reads from,
//! and hand back that id.
//!
//! The on-disk shape is not part of any public API — the SDK's own
//! `SessionStoreEntry` type is deliberately just `{type: string, ...opaque}`.
//! What is here was read off real session files
//! (`~/.claude/projects/*/*.jsonl`): every `user`/`assistant` line's key set,
//! and in particular the exact `tool_result` → `image` → `source` shape
//! (`{"type":"image","source":{"type":"base64","media_type":...,"data":...}}`)
//! a real transcript uses for a figure. `continuation()` stays as the last
//! resort for whatever this can't cover (a non-Claude engine, or a write
//! failure).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::state::NativeSession;
use super::AgentError;
use crate::agent::model::{ContentBlock, ModelMessage, ModelRole};
use crate::agent::tools::{clamp_for_model, ToolRegistry};
use crate::agent::transcript::{to_model_messages, Transcript};

/// How many trailing messages keep their tool results at full, already
/// model-clamped size (see `python.rs::model_output`, which every tool
/// result here was already built through). Everything older shrinks further:
/// a long thread's early tool text is rarely what the next turn needs, and
/// this keeps a very long history from growing the hydrated session without
/// bound.
const RECENT_MESSAGES: usize = 6;
const OLD_TOOL_TEXT_BUDGET: usize = 1_500;

/// Build a fresh native Claude session from everything in `transcript`
/// before `turn_message_id`, and write it where `claude --resume` expects to
/// find it. The turn's own new message is deliberately excluded — it is sent
/// as this call's prompt, exactly as a literal `--resume` already does.
///
/// # Errors
/// If the transcript cannot be serialized, or the session file cannot be
/// written.
pub(super) fn hydrate(
    transcript: &Transcript,
    turn_message_id: &str,
    registry: &ToolRegistry,
    model: Option<&str>,
    cwd: &Path,
) -> Result<NativeSession, AgentError> {
    let cut = transcript
        .messages
        .iter()
        .position(|m| m.id == turn_message_id)
        .unwrap_or(transcript.messages.len());
    let history = Transcript {
        messages: transcript.messages[..cut].to_vec(),
    };
    let mut messages = to_model_messages(&history, registry);
    trim_old_tool_results(&mut messages);

    let session_id = uuid::Uuid::new_v4().to_string();
    let entries = to_session_entries(&messages, &session_id, cwd, model);
    write_session(&session_id, cwd, &entries)?;
    Ok(NativeSession {
        id: session_id,
        usage: crate::agent::model::Usage::default(),
    })
}

/// Shrink every tool result's text outside the most recent turns. Left
/// alone: image blocks (a real `image` content block is exactly what makes
/// hydration worth doing — the model can actually see it) and everything
/// that is not a tool result (assistant text/tool-use input is not what grew
/// unbounded).
fn trim_old_tool_results(messages: &mut [ModelMessage]) {
    let recent_from = messages.len().saturating_sub(RECENT_MESSAGES);
    for message in &mut messages[..recent_from] {
        for block in &mut message.content {
            let ContentBlock::ToolResult { content, .. } = block else {
                continue;
            };
            for inner in content {
                if let ContentBlock::Text(text) = inner {
                    if text.chars().count() > OLD_TOOL_TEXT_BUDGET {
                        *text = clamp_for_model(text, OLD_TOOL_TEXT_BUDGET, "tool result", 0.3);
                    }
                }
            }
        }
    }
}

/// The on-disk JSONL entries for `messages`, linked by a fresh `parentUuid`
/// chain — a straight line, since this history has no forks.
fn to_session_entries(
    messages: &[ModelMessage],
    session_id: &str,
    cwd: &Path,
    model: Option<&str>,
) -> Vec<Value> {
    let cwd = cwd.to_string_lossy().into_owned();
    let now = chrono::Utc::now().to_rfc3339();
    let mut parent: Option<String> = None;
    let mut entries = Vec::with_capacity(messages.len());
    for message in messages {
        let uuid = uuid::Uuid::new_v4().to_string();
        let entry = match message.role {
            ModelRole::User => {
                user_entry(message, session_id, &cwd, &now, &uuid, parent.as_deref())
            }
            ModelRole::Assistant => assistant_entry(
                message,
                session_id,
                &cwd,
                &now,
                &uuid,
                parent.as_deref(),
                model,
            ),
        };
        entries.push(entry);
        parent = Some(uuid);
    }
    entries
}

fn user_entry(
    message: &ModelMessage,
    session_id: &str,
    cwd: &str,
    timestamp: &str,
    uuid: &str,
    parent: Option<&str>,
) -> Value {
    // A plain human turn is exactly one text block; the CLI's own files carry
    // that as a bare string rather than a one-element array.
    let content = match message.content.as_slice() {
        [ContentBlock::Text(text)] => Value::String(text.clone()),
        blocks => Value::Array(blocks.iter().map(user_block).collect()),
    };
    json!({
        "parentUuid": parent,
        "isSidechain": false,
        "type": "user",
        "message": {"role": "user", "content": content},
        "uuid": uuid,
        "timestamp": timestamp,
        "userType": "external",
        "entrypoint": "sdk-cli",
        "cwd": cwd,
        "sessionId": session_id,
        "version": env!("CARGO_PKG_VERSION"),
        "gitBranch": "HEAD",
    })
}

fn user_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text(text) => json!({"type": "text", "text": text}),
        ContentBlock::ToolResult {
            id,
            content,
            is_error,
        } => {
            let mut value = json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": content.iter().map(tool_result_block).collect::<Vec<_>>(),
            });
            if *is_error {
                value["is_error"] = Value::Bool(true);
            }
            value
        }
        // A user turn never itself opens a tool call or carries a bare image
        // in what `to_model_messages` produces — but a future content kind
        // must not silently vanish from a resumed conversation.
        other => tool_result_block(other),
    }
}

fn tool_result_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text(text) => json!({"type": "text", "text": text}),
        ContentBlock::Image { media_type, data } => json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data},
        }),
        ContentBlock::ToolUse { id, name, input } => {
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::ToolResult {
            id,
            content,
            is_error,
        } => {
            let mut value = json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": content.iter().map(tool_result_block).collect::<Vec<_>>(),
            });
            if *is_error {
                value["is_error"] = Value::Bool(true);
            }
            value
        }
    }
}

fn assistant_entry(
    message: &ModelMessage,
    session_id: &str,
    cwd: &str,
    timestamp: &str,
    uuid: &str,
    parent: Option<&str>,
    model: Option<&str>,
) -> Value {
    let content: Vec<Value> = message.content.iter().map(tool_result_block).collect();
    let stop_reason = if content.iter().any(|block| block["type"] == "tool_use") {
        "tool_use"
    } else {
        "end_turn"
    };
    json!({
        "parentUuid": parent,
        "isSidechain": false,
        "type": "assistant",
        "message": {
            "role": "assistant",
            "type": "message",
            "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
            "model": model.unwrap_or("unknown"),
            "content": content,
            "stop_reason": stop_reason,
            "stop_sequence": Value::Null,
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            },
        },
        "uuid": uuid,
        "timestamp": timestamp,
        "userType": "external",
        "entrypoint": "sdk-cli",
        "cwd": cwd,
        "sessionId": session_id,
        "version": env!("CARGO_PKG_VERSION"),
        "gitBranch": "HEAD",
    })
}

/// `CLAUDE_CONFIG_DIR`, defaulting to `~/.claude` exactly as the CLI does.
fn config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude"))
}

/// The project directory a session under `cwd` resumes from: `cwd` with
/// every `/` and `.` turned into `-`. Confirmed against this machine's own
/// `~/.claude/projects/*` names rather than assumed — the mapping is
/// undocumented and CLI-internal.
pub(super) fn project_dir(cwd: &Path) -> PathBuf {
    let sanitized: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    config_dir().join("projects").join(sanitized)
}

fn write_session(session_id: &str, cwd: &Path, entries: &[Value]) -> Result<(), AgentError> {
    let directory = project_dir(cwd);
    std::fs::create_dir_all(&directory)
        .map_err(|e| AgentError::Storage(format!("could not create {directory:?}: {e}")))?;
    let mut body = String::new();
    for entry in entries {
        body.push_str(&entry.to_string());
        body.push('\n');
    }
    let path = directory.join(format!("{session_id}.jsonl"));
    std::fs::write(&path, body)
        .map_err(|e| AgentError::Storage(format!("could not write {path:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::model::ModelRole;

    fn text(role: ModelRole, text: &str) -> ModelMessage {
        ModelMessage {
            role,
            content: vec![ContentBlock::Text(text.into())],
        }
    }

    #[test]
    fn a_plain_user_turn_is_a_bare_string_like_the_real_cli_writes() {
        let entries = to_session_entries(
            &[text(ModelRole::User, "hello venue")],
            "sess-1",
            Path::new("/tmp/venue-a"),
            None,
        );
        assert_eq!(entries[0]["type"], "user");
        assert_eq!(entries[0]["message"]["content"], "hello venue");
        assert!(entries[0]["parentUuid"].is_null());
    }

    #[test]
    fn entries_chain_by_uuid_in_order() {
        let entries = to_session_entries(
            &[text(ModelRole::User, "a"), text(ModelRole::Assistant, "b")],
            "sess-1",
            Path::new("/tmp/venue-a"),
            Some("claude-opus-5"),
        );
        let first_uuid = entries[0]["uuid"].as_str().unwrap();
        assert_eq!(entries[1]["parentUuid"].as_str().unwrap(), first_uuid);
        assert_eq!(entries[1]["message"]["model"], "claude-opus-5");
    }

    #[test]
    fn a_tool_result_image_becomes_a_real_image_block_not_text() {
        let messages = vec![ModelMessage {
            role: ModelRole::User,
            content: vec![ContentBlock::ToolResult {
                id: "call-1".into(),
                content: vec![ContentBlock::Image {
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                }],
                is_error: false,
            }],
        }];
        let entries = to_session_entries(&messages, "sess-1", Path::new("/tmp/v"), None);
        let block = &entries[0]["message"]["content"][0]["content"][0];
        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["type"], "base64");
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(block["source"]["data"], "AAAA");
    }

    #[test]
    fn old_tool_text_shrinks_but_recent_tool_text_and_every_image_do_not() {
        let big = "x".repeat(10_000);
        let tool_result = |text: &str| ModelMessage {
            role: ModelRole::User,
            content: vec![ContentBlock::ToolResult {
                id: "c".into(),
                content: vec![
                    ContentBlock::Text(text.to_string()),
                    ContentBlock::Image {
                        media_type: "image/png".into(),
                        data: "x".repeat(10_000),
                    },
                ],
                is_error: false,
            }],
        };
        let mut messages: Vec<ModelMessage> = (0..10).map(|_| tool_result(&big)).collect();
        trim_old_tool_results(&mut messages);
        let ContentBlock::ToolResult { content, .. } = &messages[0].content[0] else {
            panic!("expected a tool result");
        };
        let ContentBlock::Text(old_text) = &content[0] else {
            panic!("expected text");
        };
        assert!(
            old_text.len() < big.len(),
            "an old tool result should shrink"
        );
        let ContentBlock::Image { data, .. } = &content[1] else {
            panic!("expected an image");
        };
        assert_eq!(data.len(), 10_000, "an image is never trimmed");
        let ContentBlock::ToolResult { content, .. } = &messages[9].content[0] else {
            panic!("expected a tool result");
        };
        let ContentBlock::Text(recent_text) = &content[0] else {
            panic!("expected text");
        };
        assert_eq!(
            recent_text.len(),
            big.len(),
            "a recent tool result stays full"
        );
    }

    #[test]
    fn project_dir_matches_this_machines_real_claude_projects_naming() {
        let dir = project_dir(Path::new("/home/julian/github/luma"));
        assert!(dir.ends_with("projects/-home-julian-github-luma"));
    }

    #[test]
    fn hydrate_writes_a_session_file_at_the_resumable_path() {
        let scratch = tempfile::tempdir().unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", scratch.path());
        let cwd = tempfile::tempdir().unwrap();
        let transcript = Transcript {
            messages: vec![crate::agent::AgentChatMessage::user("u1", "hi")],
        };
        let registry = ToolRegistry::new(vec![]);
        let session = hydrate(
            &transcript,
            "turn-not-in-transcript",
            &registry,
            None,
            cwd.path(),
        )
        .unwrap();
        let path = project_dir(cwd.path()).join(format!("{}.jsonl", session.id));
        assert!(path.exists());
        let body = std::fs::read_to_string(path).unwrap();
        assert_eq!(body.lines().count(), 1);
        std::env::remove_var("CLAUDE_CONFIG_DIR");
    }
}
