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

/// What `open` pinned. Every later call addresses this thread; `python` never
/// takes a scope, exactly as the in-app tool never lets the model choose one.
#[derive(Clone, Debug)]
struct Session {
    thread_id: String,
    /// The durable user message cells are attributed to. `run_python_cell`
    /// requires one, and an edit-capable cell must be attributable to a real
    /// turn — here the MCP client is the user, and one message stands for the
    /// whole connection.
    turn_message_id: String,
    track_id: String,
    /// The room `luma.venue` describes. Always present: a session is a track
    /// *in a venue*, and the venue is resolved before the thread is pinned.
    venue_id: String,
    /// The track's membership in that venue — the authored timeline
    /// `luma.track` reads and edits. `open` binds the score the app would open
    /// (see [`bind_venue`]) and creates one when the track has none, rather
    /// than requiring the caller to have made it in the app first.
    score_id: String,
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
            "Bind a track's Luma workspace to this session, and return the catalog of \
             everything `luma` then exposes. Name the track with `track_id` or `track_query` \
             (which matches artist and title); `find` searches for both. `venue_id` picks the \
             room; without it the track's own venue is used, or the library's if there is \
             only one. Opening a venue the track is not in yet adds it, which writes. Opening \
             again replaces the session.",
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
                        "description": "Venue to bind. Defaults to the track's venue, or the library's only venue.",
                    },
                    "model": {
                        "type": "string",
                        "description": "The model driving this session, recorded as the author of every edit it makes.",
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
            "Throw the workspace and kernel away and bind the same track again. Every \
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

/// Resolve a track (and the venue/score that make `luma.venue` and
/// `luma.track` real), pin a durable thread to it, and report the catalog.
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

    let (tracks, venues) = library(services, venue_id).await?;
    let track = pick_track(&tracks, track_id, track_query)?;
    let track_id = string(&track, "id");
    let label = track_label(&track);

    let (venue_id, score_id) = bind_venue(services, &track_id, &label, venue_id, &venues).await?;

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

    // Who this session's revisions belong to. The connection already named the
    // client; the model refines it, because "Claude Code" is a program and the
    // thing that actually authored the edit is the model driving it.
    // Cloned rather than read in place: this holds across an `invoke`, and no
    // lock this process takes should span one.
    let connected = client.read().await.clone();
    if let Some(connected) = connected {
        let actor = client_actor(&connected, model);
        if let Err(error) = invoke(
            services,
            "agent_thread_set_actor",
            json!({ "threadId": thread_id, "actor": actor }),
        )
        .await
        {
            eprintln!("[luma-mcp] naming this session's writer: {error}");
        }
    }

    let appended = invoke(
        services,
        "agent_thread_append_messages",
        json!({
            "threadId": thread_id,
            "input": {
                "operationId": uuid(),
                "expectedHeadMessageId": Value::Null,
                "messages": [{ "role": "user", "parts": [
                    { "type": "text", "text": format!("MCP session on {label}.") },
                ]}],
            },
        }),
    )
    .await?;
    let turn_message_id = appended
        .as_array()
        .and_then(|messages| messages.first())
        .map(|message| string(message, "id"))
        .ok_or_else(|| format!("the session turn was not appended: {appended}"))?;

    let opened = Session {
        thread_id,
        turn_message_id,
        track_id,
        venue_id,
        score_id,
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
    Ok(format!(
        "opened {label}\ntrack {}\nvenue {}, score {}\n\n{}\n{}",
        opened.track_id,
        opened.venue_id,
        opened.score_id,
        catalog.stdout,
        skills::bundled().listing(),
    ))
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
async fn bind_venue(
    services: &AppServices,
    track_id: &str,
    label: &str,
    requested: Option<&str>,
    venues: &[Value],
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
    // An empty venue id is this command's "every venue I can see" overload.
    let scores = invoke(
        services,
        "list_scores_for_track",
        json!({ "trackId": track_id, "venueId": "" }),
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
        return tool_error("no track is open — call `open` first");
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
        return Err("no track is open — call `open` first".into());
    };
    close(services, &open).await?;
    let arguments = json!({
        "track_id": open.track_id,
        "venue_id": open.venue_id,
        "model": open.model,
    });
    self::open(services, &arguments, session, client).await
}

async fn cancel(services: &AppServices, session: &SessionCell) -> Result<String, String> {
    let Some(open) = session.read().await.clone() else {
        return Err("no track is open — call `open` first".into());
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
            "turnMessageId": session.turn_message_id,
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

async fn run() -> Result<(), String> {
    let config = HostConfig::parse_args(std::env::args().skip(1))?;
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

/// Tell the host to attribute its authored revisions to the connected client.
///
/// Best effort: a client whose name carries punctuation the actor vocabulary
/// refuses is not a reason to refuse the connection — the revisions simply stay
/// labelled as this host's default.
async fn adopt(services: &AppServices, client: &ClientInfo, label: &ClientCell) {
    let actor = client_actor(client, None);
    match invoke(
        services,
        "authored_state_set_session_actor",
        json!({ "actor": actor }),
    )
    .await
    {
        Ok(_) => *label.write().await = Some(client.clone()),
        Err(error) => eprintln!("[luma-mcp] naming the connected client: {error}"),
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
