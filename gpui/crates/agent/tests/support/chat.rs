//! One pattern, one scripted turn, and the app opened on both.
//!
//! # Why the model and the tool are scripted, and the database is real
//!
//! What is under test is the *surface*: that a delta becomes painted markdown,
//! that a tool call becomes a chip, that a turn in flight reads as one. None of
//! that wants a network, and a test that needed one would be a test of the
//! network. The turn protocol underneath is real, though — a real thread, real
//! rows, the real trigger — because the panel reads what the loop persisted and
//! a fake transcript would be the panel testing itself.
//!
//! The tool deliberately takes its time. Every event the scripted model has is
//! ready at once, so without a pause there is no moment at which a turn is
//! half-finished, and "streaming" would be a thing this suite could not see.

#![allow(dead_code)]

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{px, size, AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode};
use luma_lib::agent::model::scripted::ScriptedModel;
use luma_lib::agent::model::{ModelEvent, StopReason, Usage};
use luma_lib::agent::tools::{Tool, ToolContext, ToolRegistry};
use serde_json::{json, Value};

/// One pattern per test, plus the one the captures are taken over.
///
/// A conversation's identity is its scope, and a scope's subject is the
/// pattern — so two tests sharing a pattern would share a thread, and the
/// second would open onto the first's transcript. One pattern each is what
/// makes each test's panel empty when it opens.
pub const PATTERNS: [&str; 4] = ["chat-turn", "chat-growth", "chat-typing", CAPTURED];

/// The pattern the reference captures are taken over.
pub const CAPTURED: &str = "gauntlet-chat";

/// How long the scripted tool takes to answer.
///
/// It is the window in which a turn is observably half-finished, so it has to
/// be wide enough to survive a draw on a loaded machine — a tighter window
/// made the mid-turn assertions fail whenever something else was compiling.
/// [`UNTIL`] polls inside it rather than sleeping through it, so the suite
/// pays the latency only when a state never arrives.
const TOOL_LATENCY: Duration = Duration::from_millis(1_200);

/// What the model "says" in its first step, before it calls the tool.
pub const OPENING: &str =
    "Chasing the **downbeat**. I'll sample the ramp and check where it peaks.";

/// …and in its second, after the tool answered.
pub const CLOSING: &str = "Here is the curve it settled on:\n\n\
    ```python\n\
    for beat in range(4):\n\
    \x20   ramp(beat, 0.5)\n\
    ```\n\n\
    - peak at bar 3\n\
    - release over two bars\n";

/// A `python` tool that answers a fixed capture, slowly.
///
/// Named `python` because a tool's name is how the model addresses it and how
/// the chip narrates it: a differently-named stand-in would exercise the
/// default phrasing rather than the one the app ships.
struct ScriptedTool;

#[async_trait::async_trait]
impl Tool for ScriptedTool {
    fn name(&self) -> &'static str {
        "python"
    }

    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed("Run a Python cell against the pattern.")
    }

    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": { "code": { "type": "string" } } })
    }

    async fn call(&self, _ctx: &ToolContext<'_>, _args: Value) -> Result<Value, String> {
        tokio::time::sleep(TOOL_LATENCY).await;
        Ok(json!({ "stdout": "peak=3.0\n" }))
    }
}

/// The turn: prose, a tool call, then prose with a code block in it.
fn script() -> Vec<Vec<ModelEvent>> {
    let step = |events: Vec<ModelEvent>, stop: StopReason| {
        let mut events = events;
        events.push(ModelEvent::StepEnded {
            stop_reason: stop,
            usage: Usage::default(),
        });
        events
    };
    // Split into several deltas rather than one: the veil fades *chunks*, and a
    // single-delta reply would never exercise the path the streaming look
    // depends on.
    let chunks = |text: &str| -> Vec<ModelEvent> {
        text.as_bytes()
            .chunks(24)
            .map(|chunk| ModelEvent::TextDelta(String::from_utf8_lossy(chunk).into_owned()))
            .collect()
    };
    let mut first = chunks(OPENING);
    first.extend([
        ModelEvent::ToolCallStarted {
            id: "call-1".into(),
            name: "python".into(),
        },
        ModelEvent::ToolCallArgsDelta {
            id: "call-1".into(),
            json: r#"{"code":"ramp.peak()"}"#.into(),
        },
        ModelEvent::ToolCallEnded {
            id: "call-1".into(),
        },
    ]);
    vec![
        step(first, StopReason::ToolUse),
        step(chunks(CLOSING), StopReason::EndTurn),
    ]
}

/// A library of its own, with one pattern in it, named after the process so
/// two runs never share one.
fn config_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-chat-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("failed to create the temporary config directory");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the fixture runtime")
        .block_on(seed(&dir));
    dir
}

async fn seed(config_dir: &Path) {
    let db = luma_lib::database::local::database::init_app_db_at(config_dir)
        .await
        .expect("failed to open the fixture database");
    let state_db = luma_lib::database::local::state::init_state_db_at(config_dir)
        .await
        .expect("failed to open the fixture state database");
    luma_lib::database::local::auth::bootstrap_host_admission(&db.0, &state_db.0)
        .await
        .expect("failed to arm admission");
    let storage = luma_lib::storage::StorageRoot::from_path(config_dir.to_path_buf());
    let workspaces = Arc::new(
        luma_lib::agent_execution::workspace::PythonWorkspaceService::new(
            storage.agent_workspaces_dir(),
            Arc::new(|| Err("the fixture does not run Python workspaces".to_string())),
        ),
    );
    let services = luma_lib::dispatch::AppServices::headless(
        db,
        state_db,
        storage,
        config_dir.to_path_buf(),
        workspaces,
    );
    for (index, name) in PATTERNS.iter().enumerate() {
        let created = luma_lib::dispatch::dispatch(
            &services,
            "create_pattern",
            &json!({
                "requestId": format!("7f1c2c60-0000-4000-8000-0000000003a{index}"),
                "name": name,
                "description": null,
            }),
        )
        .await
        .expect("failed to seed the pattern");
        let id = created["id"].as_str().expect("a created pattern has an id");
        if *name == CAPTURED {
            seed_graph(&services, id).await;
        }
    }
}

/// Give the captured pattern a real graph, so the plate behind the panel is
/// the editor doing its job rather than an empty canvas. The graph is the
/// gauntlet's own `gradient` fixture — the same one the graph captures use, so
/// the two sets of plates are of one library.
async fn seed_graph(services: &luma_lib::dispatch::AppServices, pattern: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../harness/gauntlet/fixtures/gradient.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return; // no fixture checked out: the capture still works, emptier.
    };
    let fixture: Value = serde_json::from_slice(&bytes).expect("the fixture is not JSON");
    let document = luma_lib::dispatch::dispatch(
        services,
        "get_pattern_graph_document",
        &json!({ "id": pattern, "implementationId": null }),
    )
    .await
    .expect("failed to read the pattern's graph document");
    luma_lib::dispatch::dispatch(
        services,
        "save_pattern_graph_document",
        &json!({
            "id": pattern,
            "implementationId": document["implementationId"],
            "operationId": "7f1c2c60-0000-4000-8000-0000000003b0",
            "baseRevision": document["revision"],
            "graph": fixture["graph"],
        }),
    )
    .await
    .expect("failed to seed the pattern's graph");
}

/// The seeded library, made once per process.
///
/// `LUMA_CONFIG_DIR` is process-global, so there is exactly one fixture
/// library per test binary — and building it twice is what produced a locked
/// database when two tests raced into the migrations.
fn fixture() -> &'static PathBuf {
    static FIXTURE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(config_dir)
}

/// One test's exclusive turn with the fixture.
///
/// Held for the whole test rather than just for the seeding: the tests open
/// three separate `Library` handles onto one SQLite file, and letting them
/// overlap tests the database's locking rather than the panel.
pub struct Session {
    pub app: Harness,
    _gate: std::sync::MutexGuard<'static, ()>,
}

/// The app, on the seeded library, with the turn scripted.
pub fn session(mode: Mode, window: (f32, f32)) -> Session {
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let gate = GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::env::set_var("LUMA_CONFIG_DIR", fixture());
    let root: gpui_agent::RootFactory = Arc::new(|_: &mut Window, cx: &mut App| -> AnyView {
        luma_app::init(cx);
        let mut library = luma_app::Library::open().expect("failed to open the fixture library");
        library.set_agent_model(Arc::new(ScriptedModel::new(script())));
        library.set_agent_tools(ToolRegistry::new(vec![Arc::new(ScriptedTool)]));
        cx.new(|cx| luma_app::Luma::new(library, cx)).into()
    });
    let app = Harness::headless(
        Config {
            mode,
            window_size: size(px(window.0), px(window.1)),
            call_timeout: Duration::from_secs(120),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start the harness");
    Session { app, _gate: gate }
}

/// `until(pred)` in a script: settle and draw until a snapshot satisfies
/// `pred`, or fail saying what it last saw.
///
/// Polling, not sleeping: the states under test are transient, and a fixed
/// wait either lands inside the window by luck or misses it under load.
// Assigned onto the global rather than declared: the interpreter keeps one
// context per session, so a `const` would be a redeclaration the second time a
// test pastes this in.
pub const UNTIL: &str = r#"
    globalThis.until = (what, pred) => {
        let last = null;
        for (let i = 0; i < 300; i++) {
            last = app.snapshot();
            if (pred(last)) return last;
            app.frames(1, { waitMs: 10 });
        }
        throw new Error(`never saw ${what}: ` +
            JSON.stringify(last.nodes.map((n) => `${n.role}:${n.label}`)));
    };
    globalThis.chips = (s) => s.findAll({ role: "chip" }).map((n) => n.label);
"#;

/// The composer, in a script.
///
/// Found by its placeholder rather than by role alone: the graph editor paints
/// its own node arguments as inputs, so "the first input" is one of those on
/// any screen that has them.
pub fn composer() -> String {
    format!(
        r#"app.snapshot().find({{ role: "input", label: {:?} }})"#,
        luma_chat::composer::PLACEHOLDER
    )
}

/// Open one pattern and the chat over it. Leaves the panel idle, which is
/// where every capture and every assertion starts.
///
/// Every step polls rather than counting frames: the library is behind a Tokio
/// runtime gpui cannot see, so "how many frames until the list has loaded" is a
/// guess that a busy machine falsifies. The toggle is inside the loop for the
/// same reason — the chat's scope comes from the *loaded* graph document, so a
/// toggle that arrives before it does is a no-op.
pub fn open_chat(pattern: &str) -> String {
    format!(
        r#"
        {until}
        app.click(app.snapshot().find({{ role: "button", label: "Patterns" }}));
        until("the pattern list", (s) => s.find({{ role: "row", label: {pattern:?} }}));
        app.click(app.snapshot().find({{ role: "row", label: {pattern:?} }}));
        until("the chat panel", (s) => {{
            // Not merely present: the panel slides in behind a clipping
            // container, so a control that exists is not yet a control you can
            // press. Zero width is what "clipped away" looks like.
            const send = s.find({{ role: "button", label: "Send" }});
            if (send && send.bounds.width > 0) return true;
            if (!s.find({{ role: "input", label: {placeholder:?} }})) {{
                app.action("luma::ToggleAgentChat");
            }}
            return false;
        }});
        // …and let the slide land, so nothing measured after this is partly
        // clipped.
        app.frames(8, {{ waitMs: 40 }});
    "#,
        until = UNTIL,
        placeholder = luma_chat::composer::PLACEHOLDER,
    )
}

/// Type the prompt and press Send.
pub fn send() -> String {
    format!(
        r#"
        app.type({composer}, "where does the ramp peak?");
        app.frames(2);
        app.click(app.snapshot().find({{ role: "button", label: "Send" }}));
    "#,
        composer = composer()
    )
}
