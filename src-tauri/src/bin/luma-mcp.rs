//! Luma's sandboxed Python workspace, served to an out-of-process coding agent
//! over MCP.
//!
//! The in-app track copilot gets one tool — `python`, against a persistent
//! kernel whose namespace is the current track, venue, score, audio and
//! analysis products under `luma`. This binary hands that same kernel, with the
//! same bindings and the same seatbelt sandbox, to any MCP client: Claude Code
//! opens a track and then *is* the copilot, with a real interpreter instead of
//! a description of one.
//!
//! ```text
//! find   {track?, venue?}                   -> matching tracks and venues
//! open   {track_query|track_id, venue_id?}  -> the bound namespace's catalog
//! open   {venue_id}                         -> the same, for a room with no track
//! python {code}                             -> stdout/repr/traceback + figures
//! reset  {}                                 -> a fresh workspace and kernel
//! cancel {}                                 -> interrupt the cell in flight
//! skill  {name}                             -> one lighting-craft playbook
//! ```
//!
//! The kernel is half of what makes the in-app copilot good at this; the
//! playbooks are the other half, and an external agent gets both from the same
//! registry the in-app loop reads (see the "Skills" section below).
//!
//! Like `agent_harness`, this is a thin adapter over `luma_lib::dispatch` — it
//! implements no command of its own, so scope resolution, the binding manifest,
//! the write-admission gate and the interrupt ladder are the app's, not a
//! second copy. What it adds is the *session*: an MCP client has no editor to
//! read a track from, so `open` pins one durable agent thread and every later
//! call addresses it.
//!
//! A session is usually a track in a room. Naming no track opens the *room*
//! instead: a venue thread, whose namespace is `luma.venue` alone and which
//! mints no score — a score is a track's membership in a room, and there is no
//! track to have one.
//!
//! One subcommand sits beside the server, sharing its host and its admission:
//!
//! ```text
//! luma-mcp record-usage --json '<AgentThreadUsage>'
//! ```
//!
//! It exists because a session's *price* is known only after the session ends —
//! see [`record_usage`].
//!
//! stdout is the protocol. Everything diagnostic goes to stderr.

use std::sync::Arc;

use serde_json::{json, Value};
use std::io::{BufRead, Write};

use luma_lib::agent::model::ContentBlock;
use luma_lib::agent::skills::{self, SkillRegistry};
use luma_lib::agent::tools::python::{cell_content_blocks, PYTHON_TOOL_DESCRIPTION};
use luma_lib::agent::tools::skill::SKILL_TOOL_DESCRIPTION;
use luma_lib::dispatch::{self, AppServices};
use luma_lib::headless_host::{boot, HostConfig};
use luma_lib::models::agent_execution::PythonCellResult;
use mcp_stdio::{
    image_png, ok, prompt, result, route, text, tool, tool_error, ClientInfo, Routed, ServerInfo,
    Surface,
};

const SERVER: ServerInfo = ServerInfo {
    name: "luma",
    version: env!("CARGO_PKG_VERSION"),
};

/// How many library rows one listing may show before it stops being an
/// orientation surface and starts being a context dump.
const LISTING_LIMIT: usize = 60;

// -----------------------------------------------------------------------------
// Session
// -----------------------------------------------------------------------------

/// What a session is *about*: a track in a room, or the room alone.
///
/// The room is not optional in either case — `luma.venue` is bound both times,
/// and the venue is resolved before the thread is pinned. What varies is
/// whether there is a track, and therefore a score.
#[derive(Clone, Debug)]
enum Subject {
    Track { track_id: String, score_id: String },
    Venue,
}

/// What `open` pinned. Every later call addresses this thread; `python` never
/// takes a scope, exactly as the in-app tool never lets the model choose one.
#[derive(Clone, Debug)]
struct Session {
    thread_id: String,
    /// The durable session turn cells are attributed to — one row standing
    /// for the whole connection.
    ///
    /// A cell with edit authority must belong to a turn some principal opened.
    /// This client is a principal; it is not a *user*, so the turn it opens
    /// carries [`Role::Session`] and no speech. Who opened it is the thread's
    /// actor, stamped just above by `agent_thread_set_actor` — this row
    /// repeats none of it.
    ///
    /// [`Role::Session`]: luma_lib::agent::transcript::Role::Session
    session_message_id: String,
    /// The room `luma.venue` describes. Always present.
    venue_id: String,
    /// The track and its membership in that venue — the authored timeline
    /// `luma.track` reads and edits — or [`Subject::Venue`] when this session
    /// is about the room alone. `open` binds the score the app would open (see
    /// [`bind_venue`]) and creates one when the track has none, rather than
    /// requiring the caller to have made it in the app first.
    subject: Subject,
    /// What the client said is driving it, when it said. Remembered so `reset`
    /// rebinds the same writer, not just the same track.
    model: Option<String>,
}

/// The open session, or none. A read is always a clone: `python` must not hold
/// this lock while a cell runs, or `cancel` could never reach the kernel.
type SessionCell = Arc<tokio::sync::RwLock<Option<Session>>>;

/// Who connected, from the `initialize` handshake. `None` when the client sent
/// no `clientInfo` — the host's own default then answers for authorship.
type ClientCell = Arc<tokio::sync::RwLock<Option<ClientInfo>>>;

// -----------------------------------------------------------------------------
// Tools
// -----------------------------------------------------------------------------

fn tools() -> Value {
    json!([
        tool(
            "find",
            "Search the library without touching it. `track` matches a track's id, artist or \
             title; `venue` matches a venue's id or name; either may be omitted to list that \
             half whole. Reads only — use it to get the ids `open` wants.",
            &json!({
                "type": "object",
                "properties": {
                    "track": {
                        "type": "string",
                        "description": "Track id, or a substring of the artist or title.",
                    },
                    "venue": {
                        "type": "string",
                        "description": "Venue id, or a substring of the name.",
                    },
                },
            }),
        ),
        tool(
            "open",
            "Bind a Luma workspace to this session, and return the catalog of everything \
             `luma` then exposes. Name the track with `track_id` or `track_query` (which \
             matches artist and title); `find` searches for both. `venue_id` picks the room; \
             without it the track's own venue is used, or the library's if there is only one. \
             Opening a venue the track is not in yet adds it, which writes. Name *no* track \
             and the room itself is the subject: `luma.venue` is bound, `luma.track` is not \
             there at all, and no score is written. Opening again replaces the session. \
             `new_score` starts a blank score beside whatever the track already has in that \
             room instead of continuing the latest one.",
            &json!({
                "type": "object",
                "properties": {
                    "track_id": { "type": "string", "description": "Exact track id." },
                    "track_query": {
                        "type": "string",
                        "description": "Substring of the artist or title.",
                    },
                    "venue_id": {
                        "type": "string",
                        "description": "Venue to bind. Defaults to the track's venue, or the library's only venue. With no track named, this is the whole subject.",
                    },
                    "model": {
                        "type": "string",
                        "description": "The model driving this session, recorded as the author of every edit it makes.",
                    },
                    "new_score": {
                        "type": "boolean",
                        "description": "Author a fresh, empty score for this track and venue instead of continuing the one the app would open. Leaves existing scores untouched.",
                    },
                },
            }),
        ),
        tool(
            "python",
            PYTHON_TOOL_DESCRIPTION,
            &json!({
                "type": "object",
                "properties": { "code": { "type": "string", "description": "Python cell source." } },
                "required": ["code"],
            })
        ),
        tool(
            "reset",
            "Throw the workspace and kernel away and bind the same subject again. Every \
             variable, import and staged edit is lost.",
            &json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "cancel",
            "Interrupt the cell running in this session's kernel. Answers whether there was \
             one.",
            &json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "skill",
            &skill_tool_description(),
            &json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Skill name from the list above." },
                },
                "required": ["name"],
            })
        ),
    ])
}

async fn call(
    services: &AppServices,
    name: &str,
    arguments: &Value,
    session: &SessionCell,
    client: &ClientCell,
) -> Value {
    let outcome = match name {
        "find" => find(services, arguments).await,
        "open" => open(services, arguments, session, client).await,
        "python" => {
            let Some(code) = arguments.get("code").and_then(Value::as_str) else {
                return tool_error("python: `code` is required and must be a string");
            };
            return python(services, code, session).await;
        }
        "reset" => reset(services, session, client).await,
        "cancel" => cancel(services, session).await,
        "skill" => skill(arguments),
        other => Err(format!("unknown tool: {other}")),
    };
    match outcome {
        Ok(body) => result(&[text(body)], false),
        Err(error) => tool_error(error),
    }
}

/// Resolve the subject — a track in a room, or a room on its own — pin a
/// durable thread to it, and report the catalog.
///
/// Naming no track is not a degenerate open: it is the room's own session. It
/// mints no score, so it is the only `open` that writes nothing to the library
/// at all, and it is the only one that works against a library with no tracks
/// in it.
async fn open(
    services: &AppServices,
    arguments: &Value,
    session: &SessionCell,
    client: &ClientCell,
) -> Result<String, String> {
    let track_id = arguments.get("track_id").and_then(Value::as_str);
    let track_query = arguments.get("track_query").and_then(Value::as_str);
    let venue_id = arguments.get("venue_id").and_then(Value::as_str);
    let model = arguments.get("model").and_then(Value::as_str);
    let new_score = arguments
        .get("new_score")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let (tracks, venues) = library(services, venue_id).await?;
    if track_id.is_none() && track_query.is_none() {
        return open_venue(services, venue_id, &venues, model, session, client).await;
    }
    let track = pick_track(&tracks, track_id, track_query)?;
    let track_id = string(&track, "id");
    let label = track_label(&track);

    // Who this session's revisions belong to. The connection already named the
    // client; the model refines it, because "Claude Code" is a program and the
    // thing that actually authored the edit is the model driving it.
    // Cloned rather than read in place: this holds across an `invoke`, and no
    // lock this process takes should span one.
    //
    // Refined *before* the score is bound, because minting one is itself an
    // authored revision and it carries the host's session actor — a score this
    // client created should not read as the operator's, or as a nameless
    // client's.
    let connected = client.read().await.clone();
    if let (Some(connected), Some(model)) = (&connected, model) {
        set_actor(
            services,
            "authored_state_set_session_actor",
            json!({ "actor": client_actor(connected, Some(model)) }),
        )
        .await;
    }

    let (venue_id, score_id) =
        bind_venue(services, &track_id, &label, venue_id, &venues, new_score).await?;

    let thread = invoke(
        services,
        "agent_thread_create",
        json!({ "input": {
            "requestId": uuid(),
            "agentKind": "track_copilot",
            "subjectKind": "track",
            "subjectId": track_id,
            "venueId": venue_id,
            "scoreId": score_id,
            "title": format!("mcp: {label}"),
        }}),
    )
    .await?;
    let thread_id = string(&thread, "id");

    if let Some(connected) = &connected {
        set_actor(
            services,
            "agent_thread_set_actor",
            json!({ "threadId": thread_id, "actor": client_actor(connected, model) }),
        )
        .await;
    }

    let session_message_id = session_turn(services, &thread_id, &label).await?;

    let opened = Session {
        thread_id,
        session_message_id,
        venue_id,
        subject: Subject::Track { track_id, score_id },
        model: model.map(str::to_owned),
    };
    let previous = session.write().await.replace(opened.clone());
    if let Some(previous) = previous {
        // Best effort: a leftover thread costs a workspace directory, not
        // correctness, and refusing to open because the last one would not
        // close would be the worse failure.
        if let Err(error) = close(services, &previous).await {
            eprintln!("[luma-mcp] closing the previous session: {error}");
        }
    }

    // Printed, not evaluated: the catalog is a multi-line string, and its repr
    // would arrive as one escaped line.
    let catalog = cell(services, &opened, "print(luma.catalog())").await?;
    if catalog.status != "ok" {
        return Err(format!(
            "the kernel could not bind {label}: {}",
            catalog.traceback.unwrap_or(catalog.stderr)
        ));
    }
    // `open` is this server's system prompt: it is the first call every client
    // makes, and the only place a listing reaches a model that has no Luma
    // prompt of its own.
    // The thread is named as well as the score because it is the only handle a
    // caller outside this process has on what the session *cost*: the harness
    // that spawned the client learns the price after the client hangs up, by
    // which time this server is gone, and `record-usage` needs an id.
    let Subject::Track { track_id, score_id } = &opened.subject else {
        unreachable!("this path binds a track");
    };
    Ok(format!(
        "opened {label}\ntrack {track_id}\nvenue {}, score {score_id}\nthread {}\n\n{}\n{}",
        opened.venue_id,
        opened.thread_id,
        catalog.stdout,
        skills::bundled().listing(),
    ))
}

/// Open the room itself: a venue thread, bound to `luma.venue` and nothing
/// else.
///
/// The venue must be named, or be the library's only one. There is no track to
/// infer it from, and guessing at which room a rig-building session is about
/// would be the worst possible thing to be wrong about.
async fn open_venue(
    services: &AppServices,
    requested: Option<&str>,
    venues: &[Value],
    model: Option<&str>,
    session: &SessionCell,
    client: &ClientCell,
) -> Result<String, String> {
    let venue = match (requested, venues) {
        (Some(requested), _) => venues
            .iter()
            .find(|venue| string(venue, "id") == requested)
            .ok_or_else(|| format!("no venue with id '{requested}'\n{}", venue_list(venues)))?,
        (None, [only]) => only,
        (None, []) => return Err("the library has no venue — create one in the app first".into()),
        (None, many) => {
            return Err(format!(
                "name the venue to open with `venue_id`\n{}",
                venue_list(many)
            ))
        }
    };
    let venue_id = string(venue, "id");
    let label = string(venue, "name");

    let connected = client.read().await.clone();
    let thread = invoke(
        services,
        "agent_thread_create",
        json!({ "input": {
            "requestId": uuid(),
            "agentKind": "venue_rig",
            "subjectKind": "venue",
            "subjectId": venue_id,
            "venueId": venue_id,
            "title": format!("mcp: {label}"),
        }}),
    )
    .await?;
    let thread_id = string(&thread, "id");
    if let Some(connected) = &connected {
        set_actor(
            services,
            "agent_thread_set_actor",
            json!({ "threadId": thread_id, "actor": client_actor(connected, model) }),
        )
        .await;
    }

    let session_message_id = session_turn(services, &thread_id, &label).await?;
    let opened = Session {
        thread_id,
        session_message_id,
        venue_id,
        subject: Subject::Venue,
        model: model.map(str::to_owned),
    };
    let previous = session.write().await.replace(opened.clone());
    if let Some(previous) = previous {
        if let Err(error) = close(services, &previous).await {
            eprintln!("[luma-mcp] closing the previous session: {error}");
        }
    }

    let catalog = cell(services, &opened, "print(luma.catalog())").await?;
    if catalog.status != "ok" {
        return Err(format!(
            "the kernel could not bind {label}: {}",
            catalog.traceback.unwrap_or(catalog.stderr)
        ));
    }
    Ok(format!(
        "opened {label}\nvenue {}\nthread {}\n\n{}\n{}",
        opened.venue_id,
        opened.thread_id,
        catalog.stdout,
        skills::bundled().listing(),
    ))
}

/// Open the session's turn: the one row every later cell is attributed to.
///
/// [`Role::Session`] and no speech — this client is a principal but not a
/// *user*, and who opened it is the thread's actor, not a sentence in the
/// transcript.
///
/// [`Role::Session`]: luma_lib::agent::transcript::Role::Session
async fn session_turn(
    services: &AppServices,
    thread_id: &str,
    label: &str,
) -> Result<String, String> {
    let appended = invoke(
        services,
        "agent_thread_append_messages",
        json!({
            "threadId": thread_id,
            "input": {
                "operationId": uuid(),
                "expectedHeadMessageId": Value::Null,
                "messages": [{ "role": "session", "parts": [
                    { "type": "text", "text": format!("Session opened on {label}.") },
                ]}],
            },
        }),
    )
    .await?;
    appended
        .as_array()
        .and_then(|messages| messages.first())
        .map(|message| string(message, "id"))
        .ok_or_else(|| format!("the session turn was not appended: {appended}"))
}

/// The room this session binds, and the track's membership in it.
///
/// A venue is describable on its own — `luma.venue` needs no score — but the
/// durable thread that owns the kernel is scoped to one authored document, so a
/// session always carries the score too.
///
/// A track can hold more than one score in the same venue, so which one `open`
/// binds has to be *the app's* answer, not a second rule: the editor opens
/// `list_scores_for_track`'s first row, and that command orders by
/// `updated_at DESC`, so the most recently worked-on score is the one the
/// operator sees and the one this session gets. Only when the track has no
/// membership at all does `ensure_venue_score` mint one — it is idempotent, so
/// opening the same fresh pair twice still binds a single score.
///
/// `new_score` is the other intent: not "the score the operator sees" but "a
/// blank one of my own". A track may hold many scores in a room, so this adds
/// rather than replaces — nothing already authored is touched.
async fn bind_venue(
    services: &AppServices,
    track_id: &str,
    label: &str,
    requested: Option<&str>,
    venues: &[Value],
    new_score: bool,
) -> Result<(String, String), String> {
    let venue_id = match requested {
        Some(requested) => {
            if !venues.iter().any(|venue| string(venue, "id") == requested) {
                return Err(format!(
                    "no venue with id '{requested}'\n{}",
                    venue_list(venues)
                ));
            }
            requested.to_string()
        }
        None => default_venue(services, track_id, label, venues).await?,
    };
    if new_score {
        // Named by nobody: a score's title is the operator's to give, and a
        // run that invented one would be guessing at the show it is about to
        // author. The app shows an unnamed score by its track.
        let score = invoke(
            services,
            "create_score",
            json!({
                "requestId": uuid(),
                "trackId": track_id,
                "venueId": venue_id,
                "name": Value::Null,
            }),
        )
        .await?;
        return Ok((venue_id, string(&score, "id")));
    }
    let existing = invoke(
        services,
        "list_scores_for_track",
        json!({ "trackId": track_id, "venueId": venue_id }),
    )
    .await?;
    if let Some(score) = existing.as_array().and_then(|scores| scores.first()) {
        return Ok((venue_id, string(score, "id")));
    }
    let score = invoke(
        services,
        "ensure_venue_score",
        json!({
            "requestId": uuid(),
            "trackId": track_id,
            "venueId": venue_id,
            "name": Value::Null,
        }),
    )
    .await?;
    Ok((venue_id, string(&score, "id")))
}

/// The venue an unqualified `open` means: the one this track is already in, or
/// the only one there is. Anything else is genuinely ambiguous and says so.
async fn default_venue(
    services: &AppServices,
    track_id: &str,
    label: &str,
    venues: &[Value],
) -> Result<String, String> {
    let scores = invoke(
        services,
        "list_scores_across_venues",
        json!({ "trackId": track_id }),
    )
    .await?;
    let mut owning: Vec<String> = scores
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|score| score.get("venueId").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    owning.sort();
    owning.dedup();

    match (owning.as_slice(), venues) {
        ([only], _) => Ok(only.clone()),
        ([], [only]) => Ok(string(only, "id")),
        ([], []) => Err("the library has no venue — create one in the app first".into()),
        ([], _) => Err(format!(
            "'{label}' is not in a venue yet; pass venue_id:\n{}",
            venue_list(venues)
        )),
        (many, _) => Err(format!(
            "'{label}' is in {} venues; pass venue_id:\n{}",
            many.len(),
            venue_list(
                &venues
                    .iter()
                    .filter(|venue| many.contains(&string(venue, "id")))
                    .cloned()
                    .collect::<Vec<Value>>()
            )
        )),
    }
}

fn venue_list(venues: &[Value]) -> String {
    venues
        .iter()
        .map(|venue| format!("  {}  {}", string(venue, "id"), string(venue, "name")))
        .collect::<Vec<String>>()
        .join("\n")
}

async fn python(services: &AppServices, code: &str, session: &SessionCell) -> Value {
    let Some(open) = session.read().await.clone() else {
        return tool_error("nothing is open — call `open` first");
    };
    match cell(services, &open, code).await {
        Err(error) => tool_error(error),
        Ok(outcome) => {
            let is_error = outcome.status != "ok";
            let content: Vec<Value> = cell_content_blocks(outcome)
                .into_iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(body) => Some(text(body)),
                    ContentBlock::Image { data, .. } => Some(image_png(data)),
                    // The projection produces only text and images; the model
                    // types' other variants belong to a transcript.
                    _ => None,
                })
                .collect();
            result(&content, is_error)
        }
    }
}

async fn reset(
    services: &AppServices,
    session: &SessionCell,
    client: &ClientCell,
) -> Result<String, String> {
    let Some(open) = session.write().await.take() else {
        return Err("nothing is open — call `open` first".into());
    };
    close(services, &open).await?;
    let arguments = match &open.subject {
        Subject::Track { track_id, .. } => json!({
            "track_id": track_id,
            "venue_id": open.venue_id,
            "model": open.model,
        }),
        // No track key at all: that absence is what `open` reads as "the room".
        Subject::Venue => json!({
            "venue_id": open.venue_id,
            "model": open.model,
        }),
    };
    self::open(services, &arguments, session, client).await
}

async fn cancel(services: &AppServices, session: &SessionCell) -> Result<String, String> {
    let Some(open) = session.read().await.clone() else {
        return Err("nothing is open — call `open` first".into());
    };
    let interrupted = invoke(
        services,
        "cancel_python_cell",
        json!({ "threadId": open.thread_id }),
    )
    .await?;
    Ok(if interrupted == json!(true) {
        "interrupted the running cell".into()
    } else {
        "there was no cell running".into()
    })
}

/// Run one cell in the session's kernel.
async fn cell(
    services: &AppServices,
    session: &Session,
    code: &str,
) -> Result<PythonCellResult, String> {
    let outcome = invoke(
        services,
        "run_python_cell",
        json!({
            "threadId": session.thread_id,
            "turnMessageId": session.session_message_id,
            "code": code,
            "scope": {},
        }),
    )
    .await?;
    serde_json::from_value(outcome).map_err(|error| format!("unreadable cell result: {error}"))
}

/// Delete the durable thread, which is what takes its workspace and kernel with
/// it.
async fn close(services: &AppServices, session: &Session) -> Result<(), String> {
    invoke(
        services,
        "agent_thread_delete",
        json!({ "threadId": session.thread_id }),
    )
    .await
    .map(|_| ())
}

// -----------------------------------------------------------------------------
// Skills
// -----------------------------------------------------------------------------

/// The `skill` tool's description: the short instruction plus the whole
/// `<available_skills>` listing.
///
/// The in-app loop puts that listing in its system prompt, which an MCP client
/// does not have — the tool list is the only text this server can rely on being
/// in a model's context, so the menu goes there.
fn skill_tool_description() -> String {
    let listing = skills::bundled().listing();
    let head = SKILL_TOOL_DESCRIPTION.trim_end();
    if listing.is_empty() {
        head.to_string()
    } else {
        format!("{head}\n\n{listing}")
    }
}

/// One playbook, in the same envelope the in-app tool returns.
fn skill(arguments: &Value) -> Result<String, String> {
    let name = arguments
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let registry = skills::bundled();
    registry
        .get(name)
        .map(skills::Skill::envelope)
        .ok_or_else(|| {
            format!(
                "unknown skill '{name}'. Available: {}",
                registry.names().join(", ")
            )
        })
}

/// Every playbook, also as an MCP prompt.
///
/// Free, and Claude Code turns each one into `/mcp__luma__<name>` in its slash
/// menu — the human half of the same surface the `skill` tool gives the model.
/// The body is the same envelope, so the two paths cannot say different things.
fn prompts(registry: &SkillRegistry) -> Value {
    Value::Array(
        registry
            .iter()
            .map(|skill| prompt(&skill.name, &skill.description, &skill.envelope()))
            .collect(),
    )
}

// -----------------------------------------------------------------------------
// Library lookup
// -----------------------------------------------------------------------------

/// Every track and venue one lookup sees. `venue_id` scopes the enriched track
/// rows the way the app's own library view does; `None` is every track.
async fn library(
    services: &AppServices,
    venue_id: Option<&str>,
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let tracks = invoke(
        services,
        "list_tracks_enriched",
        json!({ "venueId": venue_id }),
    )
    .await?;
    let venues = invoke(services, "list_venues", json!({})).await?;
    Ok((
        tracks.as_array().cloned().unwrap_or_default(),
        venues.as_array().cloned().unwrap_or_default(),
    ))
}

/// Search, and nothing else.
///
/// `open` binds a session — it pins a thread and, for a track new to the room,
/// mints the score, so it authors. A caller that only wants an id must have a
/// surface that cannot write, or every lookup leaves a revision behind.
async fn find(services: &AppServices, arguments: &Value) -> Result<String, String> {
    let track = arguments.get("track").and_then(Value::as_str);
    let venue = arguments.get("venue").and_then(Value::as_str);
    let (tracks, venues) = library(services, None).await?;
    Ok(listing(
        &retain(tracks, track, track_label),
        &retain(venues, venue, |venue| string(venue, "name")),
    ))
}

/// Rows whose id is exactly the query, or whose label contains it, case-blind.
/// No query keeps everything: an omitted half of `find` lists that half whole.
fn retain(rows: Vec<Value>, query: Option<&str>, label: fn(&Value) -> String) -> Vec<Value> {
    let Some(query) = query else { return rows };
    let needle = query.to_lowercase();
    rows.into_iter()
        .filter(|row| string(row, "id") == query || label(row).to_lowercase().contains(&needle))
        .collect()
}

fn pick_track(
    tracks: &[Value],
    track_id: Option<&str>,
    track_query: Option<&str>,
) -> Result<Value, String> {
    if let Some(track_id) = track_id {
        return tracks
            .iter()
            .find(|track| string(track, "id") == track_id)
            .cloned()
            .ok_or_else(|| format!("no track with id '{track_id}'"));
    }
    let Some(query) = track_query else {
        return Err("name a track: `track_id` or `track_query` — `find` searches for both".into());
    };
    let needle = query.to_lowercase();
    let matches: Vec<&Value> = tracks
        .iter()
        .filter(|track| track_label(track).to_lowercase().contains(&needle))
        .collect();
    match matches.as_slice() {
        [] => Err(format!("no track matches '{query}'")),
        [only] => Ok((*only).clone()),
        many => Err(format!(
            "'{query}' matches {} tracks; pass track_id:\n{}",
            many.len(),
            track_list(many.iter().copied())
        )),
    }
}

/// `  <id>  <label>` per row, capped. Ids first so a caller — human or script —
/// reads the same shape everywhere a list of tracks appears.
fn track_list<'a>(tracks: impl Iterator<Item = &'a Value>) -> String {
    tracks
        .take(LISTING_LIMIT)
        .map(|track| format!("  {}  {}", string(track, "id"), track_label(track)))
        .collect::<Vec<String>>()
        .join("\n")
}

fn listing(tracks: &[Value], venues: &[Value]) -> String {
    let mut out = format!("{} tracks", tracks.len());
    if tracks.len() > LISTING_LIMIT {
        out.push_str(&format!(" (first {LISTING_LIMIT}; narrow with `track`)"));
    }
    out.push_str(":\n");
    out.push_str(&track_list(tracks.iter()));
    out.push_str(&format!("\n\n{} venues:\n", venues.len()));
    out.push_str(&venue_list(venues));
    out.push('\n');
    out
}

fn track_label(track: &Value) -> String {
    let title = track
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("(untitled)");
    match track.get("artist").and_then(Value::as_str) {
        Some(artist) if !artist.is_empty() => format!("{artist} — {title}"),
        _ => title.to_string(),
    }
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

async fn invoke(services: &AppServices, cmd: &str, args: Value) -> Result<Value, String> {
    dispatch::dispatch(services, cmd, &args)
        .await
        .map_err(|error| format!("{cmd}: {}", String::from(error)))
}

// -----------------------------------------------------------------------------
// Main loop
// -----------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("[luma-mcp] fatal: {error}");
        std::process::exit(1);
    }
}

/// `luma-mcp record-usage --json '<AgentThreadUsage>' [host flags]`.
///
/// The session that spent the money is over by the time anyone knows what it
/// cost: `open` retires its thread when the client disconnects, and the harness
/// only reads the price out of the CLI's final event after that. So the record
/// arrives through a second, short-lived process against the same library,
/// through the same dispatcher and the same write admission.
///
/// It takes the *record*, not the harness's own result event. Two harnesses now
/// feed this — Claude Code and Codex — and their result events agree about
/// nothing; the ledger row is the contract they share, and teaching this binary
/// to parse each CLI's JSON would put the harness's knowledge in the app.
async fn record_usage(args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut json: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut args = args;
    while let Some(arg) = args.next() {
        if arg == "--json" {
            json = Some(args.next().ok_or("--json requires a value")?);
        } else {
            rest.push(arg);
        }
    }
    let json = json.ok_or("record-usage requires --json '<usage record>'")?;
    let usage: Value =
        serde_json::from_str(&json).map_err(|error| format!("--json is not JSON: {error}"))?;
    let config = HostConfig::parse_args(rest.into_iter())?;
    let services = boot(&config).await?.into_shared();
    invoke(
        &services,
        "agent_thread_record_usage",
        json!({ "usage": usage }),
    )
    .await?;
    println!("recorded usage for thread {}", string(&usage, "threadId"));
    Ok(())
}

async fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|arg| arg == "record-usage") {
        args.next();
        return record_usage(args).await;
    }
    let config = HostConfig::parse_args(args)?;
    // `into_shared`, not a bare `Arc::new`: the turn loop outlives the command
    // that starts it, so it needs the back-reference `into_shared` attaches.
    let services = boot(&config).await?.into_shared();
    eprintln!(
        "[luma-mcp] ready: config={} fixtures={}",
        services.storage().path().display(),
        services.fixtures_root().display()
    );

    let session: SessionCell = Arc::new(tokio::sync::RwLock::new(None));
    // Who connected, once `initialize` says. `open` refines the label with the
    // model the client names, per session.
    let client_cell: ClientCell = Arc::new(tokio::sync::RwLock::new(None));
    let tools = Arc::new(tools());
    let prompts = prompts(skills::bundled());
    let surface = Surface::new(&tools).with_prompts(&prompts);
    let stdout = Arc::new(tokio::sync::Mutex::new(std::io::stdout()));

    // Concurrent, one task per request, for the same reason the JSON-RPC
    // harness is: `cancel` exists precisely to interrupt a `python` that is
    // still in flight, and a serial loop could never deliver it.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    eprintln!("[luma-mcp] stdin read failed: {error}");
                    return;
                }
            }
        }
    });

    while let Some(line) = rx.recv().await {
        let (id, name, arguments) = match route(&line, SERVER, &surface) {
            Routed::Silent => continue,
            Routed::Respond { response, client } => {
                // The handshake is where this process learns who it is writing
                // for. Everything it authors from here — including the score
                // `open` mints for a track that has none — is the client's
                // work, not the operator's.
                if let Some(connected) = client {
                    adopt(&services, &connected, &client_cell).await;
                }
                write(&stdout, &response).await;
                continue;
            }
            Routed::Call {
                id,
                name,
                arguments,
            } => (id, name, arguments),
        };
        let services = services.clone();
        let session = Arc::clone(&session);
        let client_cell = Arc::clone(&client_cell);
        let stdout = Arc::clone(&stdout);
        tokio::spawn(async move {
            let outcome = call(&services, &name, &arguments, &session, &client_cell).await;
            write(&stdout, &ok(&id, &outcome)).await;
        });
    }

    // A client that hung up mid-cell leaves a kernel with nothing to answer to.
    if let Some(open) = session.write().await.take() {
        if let Err(error) = close(&services, &open).await {
            eprintln!("[luma-mcp] closing the session: {error}");
        }
    }
    Ok(())
}

/// How an external client is named in authored history: `client:<name>/<version>`,
/// refined to `client:<name>/<version>:<model>` once a session says which model
/// is driving. One function because both the handshake and `open` compose it,
/// and a revision's actor is only useful if the same client reads the same.
fn client_actor(client: &ClientInfo, model: Option<&str>) -> String {
    match model {
        Some(model) => format!("client:{}/{}:{model}", client.name, client.version),
        None => format!("client:{}/{}", client.name, client.version),
    }
}

/// Tell the host to attribute its authored revisions to the connected client,
/// and remember the client only if that took.
async fn adopt(services: &AppServices, client: &ClientInfo, label: &ClientCell) {
    let actor = client_actor(client, None);
    if set_actor(
        services,
        "authored_state_set_session_actor",
        json!({ "actor": actor }),
    )
    .await
    {
        *label.write().await = Some(client.clone());
    }
}

/// Point one of the host's actor commands at a name, and say whether it took.
///
/// Best effort by contract: a client whose name carries punctuation the actor
/// vocabulary refuses is not a reason to refuse the connection or the session —
/// the revisions simply stay labelled as this host's default.
async fn set_actor(services: &AppServices, command: &str, arguments: Value) -> bool {
    match invoke(services, command, arguments).await {
        Ok(_) => true,
        Err(error) => {
            eprintln!("[luma-mcp] naming the writer: {error}");
            false
        }
    }
}

async fn write(stdout: &tokio::sync::Mutex<std::io::Stdout>, message: &Value) {
    let Ok(mut buf) = serde_json::to_vec(message) else {
        eprintln!("[luma-mcp] response was not serializable");
        return;
    };
    buf.push(b'\n');
    let mut out = stdout.lock().await;
    if let Err(error) = out.write_all(&buf).and_then(|()| out.flush()) {
        eprintln!("[luma-mcp] stdout write failed: {error}");
    }
}
