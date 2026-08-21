//! The agent chat's exit gate: a turn, seen from the outside.
//!
//! ```sh
//! cargo test -p gpui-agent --all-features --test agent_chat
//! ```
//!
//! Every assertion here is on the *automation tree*, not on internal state —
//! the panel is only correct if a driver that can see nothing but roles and
//! labels can tell that prose arrived, that a tool ran, and that a turn was in
//! flight while both were true.

#![cfg(feature = "app")]

use std::time::Duration;

use gpui_agent::Mode;
use serde_json::Value;

#[path = "support/chat.rs"]
mod chat;

/// Wide enough that the graph screen and the 420px panel both have room; the
/// panel clips a fixed-width inner, so a narrower window would crop the thing
/// under test rather than reflow it.
const WINDOW: (f32, f32) = (1400., 900.);

fn run(session: &mut chat::Session, code: &str) -> Value {
    let result = session.app.exec(code, Duration::from_secs(120));
    assert_eq!(
        result.error, None,
        "script failed:\n{code}\n{}",
        result.stdout
    );
    result.result
}

fn labels(nodes: &Value, role: &str) -> Vec<String> {
    nodes
        .as_array()
        .expect("the script returned an array of nodes")
        .iter()
        .filter(|node| node["role"] == role)
        .map(|node| node["label"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The whole gate, in one session: idle, mid-turn, settled.
///
/// One test rather than three because the three states are one turn, and a
/// turn cannot be resumed — splitting them would mean three fixtures scripting
/// the same reply and three chances for them to disagree about it.
#[test]
fn a_turn_streams_markdown_and_shows_its_tool_call() {
    let mut session = chat::session(Mode::Headless, WINDOW);

    // -- idle -----------------------------------------------------------------
    let idle = run(
        &mut session,
        &format!("{}\napp.snapshot().nodes", chat::open_chat("chat-turn")),
    );
    assert!(
        labels(&idle, "text").iter().any(|l| l == "Pattern agent"),
        "the panel did not open: {:?}",
        labels(&idle, "text")
    );
    assert_eq!(labels(&idle, "chip"), Vec::<String>::new());
    assert!(
        labels(&idle, "button").iter().any(|l| l == "Send"),
        "no send button"
    );

    // -- mid-turn -------------------------------------------------------------
    // The scripted tool holds the turn open; two waited frames land inside it.
    let streaming = run(
        &mut session,
        &format!(
            r#"
            {send}
            until("the tool call start", (s) => chips(s).some((c) => c.startsWith("Running"))).nodes
        "#,
            send = chat::send(),
        ),
    );
    let text = labels(&streaming, "text");
    assert!(
        text.iter().any(|l| l.contains("Chasing the downbeat")),
        "the streamed prose is not painted yet: {text:?}"
    );
    assert!(
        text.iter().any(|l| l == "Working"),
        "the turn does not read as running: {text:?}"
    );
    assert_eq!(
        labels(&streaming, "chip"),
        vec!["Running python cell · ramp.peak()"],
        "the tool call has no chip"
    );

    // -- settled --------------------------------------------------------------
    let settled = run(
        &mut session,
        r#"
            until("the turn end", (s) => chips(s).some((c) => c.startsWith("Ran"))
                && !s.findAll({ role: "text" }).some((n) => n.label === "Working")).nodes
        "#,
    );
    let text = labels(&settled, "text");
    assert!(
        text.iter().any(|l| l.contains("release over two bars")),
        "the second step never landed: {text:?}"
    );
    assert!(
        text.iter().any(|l| l.contains("ramp(beat, 0.5)")),
        "the code block is not painted: {text:?}"
    );
    assert_eq!(
        labels(&settled, "chip"),
        vec!["Ran python cell · ramp.peak()"],
        "the chip did not settle"
    );
    assert!(
        !text.iter().any(|l| l == "Working"),
        "the turn never ended: {text:?}"
    );
}

/// The rendered prose grows while the turn runs. Two snapshots that differ are
/// the only honest proof this is streaming and not one final paint.
#[test]
fn the_transcript_grows_between_frames() {
    let mut session = chat::session(Mode::Headless, WINDOW);
    let lengths = run(
        &mut session,
        &format!(
            r#"
            {}
            {}
            const prose = (s) => s.findAll({{ role: "text" }})
                .map((n) => n.label).join("\n").length;
            const first = prose(app.snapshot());
            const last = prose(until("the turn end",
                (s) => !s.findAll({{ role: "text" }}).some((n) => n.label === "Working")));
            ({{ first, last }})
        "#,
            chat::open_chat("chat-growth"),
            chat::send(),
        ),
    );
    let first = lengths["first"].as_u64().expect("a length");
    let last = lengths["last"].as_u64().expect("a length");
    assert!(
        last > first,
        "the transcript did not grow across the turn: {first} then {last}"
    );
}

/// The key opens the panel on a screen that is about nothing.
///
/// The bug this gate exists for: `scope_for` answered `None` on four of the six
/// screens and the toggle returned without doing anything, so the key read as
/// broken everywhere except the graph. The panel now opens unattached and says
/// what it could attach to — which is the assertion, because "it opened" and
/// "it opened and explained itself" are different outcomes and only the second
/// one is any use to the person who pressed the key.
///
/// Both scopeless screens the chat fixture can reach, in one session: the venue
/// grid the app starts on, and the pattern list. Navigating between them keeps
/// the panel, because neither names a subject and there is nothing to re-point.
#[test]
fn the_chat_opens_unattached_on_a_screen_with_no_subject() {
    let mut session = chat::session(Mode::Headless, WINDOW);
    let welcome = run(
        &mut session,
        &format!(
            r#"
            {until}
            app.action("luma::ToggleAgentChat");
            until("the unattached panel", (s) => {{
                const header = s.findAll({{ role: "text" }}).some((n) => n.label === "Agent");
                return header && s.find({{ role: "text", label: {blurb:?} }});
            }});
            app.frames(8, {{ waitMs: 40 }});
            app.snapshot().nodes
        "#,
            until = chat::UNTIL,
            blurb = chat::UNATTACHED_BLURB,
        ),
    );
    let text = labels(&welcome, "text");
    assert!(
        text.iter().any(|l| l == "Agent"),
        "the panel did not open on the venue grid: {text:?}"
    );
    assert!(
        text.iter().any(|l| l == chat::UNATTACHED_BLURB),
        "the panel opened without saying what it attaches to: {text:?}"
    );
    assert!(
        text.iter().any(|l| l == luma_chat::TOGGLE_CHORD),
        "the panel opened without naming the chord that hides it again: {text:?}"
    );
    // No composer and no suggestions: there is no thread for either to reach,
    // and a live field over a conversation that cannot exist is the no-op
    // moved one layer in. (The venue grid's own buttons are still there, so
    // this is about the panel's, by name.)
    assert!(
        !labels(&welcome, "button").iter().any(|l| l == "Send"),
        "an unattached panel offered a send that cannot land: {:?}",
        labels(&welcome, "button")
    );
    assert!(
        !labels(&welcome, "input")
            .iter()
            .any(|l| l == luma_chat::composer::PLACEHOLDER),
        "an unattached panel offered a composer"
    );

    // …and it survives a move to another screen that is equally about nothing.
    let patterns = run(
        &mut session,
        r#"
            app.click(app.snapshot().find({ role: "button", label: "Patterns" }));
            until("the pattern list", (s) => s.find({ role: "row", label: "chat-turn" }));
            app.frames(4, { waitMs: 20 });
            app.snapshot().nodes
        "#,
    );
    assert!(
        labels(&patterns, "text").iter().any(|l| l == "Agent"),
        "the panel was dropped moving between two scopeless screens: {:?}",
        labels(&patterns, "text")
    );
}

/// An open panel follows the eye onto the screen's subject rather than
/// vanishing from under it: unattached on the pattern list, the pattern agent
/// once a pattern is open.
#[test]
fn an_open_panel_re_points_at_the_screen_it_lands_on() {
    let mut session = chat::session(Mode::Headless, WINDOW);
    let attached = run(
        &mut session,
        &format!(
            r#"
            {until}
            app.click(app.snapshot().find({{ role: "button", label: "Patterns" }}));
            until("the pattern list", (s) => s.find({{ role: "row", label: "chat-repoint" }}));
            app.action("luma::ToggleAgentChat");
            until("the unattached panel", (s) =>
                s.findAll({{ role: "text" }}).some((n) => n.label === "Agent"));
            app.click(app.snapshot().find({{ role: "row", label: "chat-repoint" }}));
            until("the pattern agent", (s) => {{
                const send = s.find({{ role: "button", label: "Send" }});
                return send && send.bounds.width > 0;
            }}).nodes
        "#,
            until = chat::UNTIL,
        ),
    );
    let text = labels(&attached, "text");
    assert!(
        text.iter().any(|l| l == "Pattern agent"),
        "the panel did not re-point onto the graph's scope: {text:?}"
    );
    assert!(
        !text.iter().any(|l| l == "Agent"),
        "the unattached header is still up beside the attached one: {text:?}"
    );
}

/// The composer declares `TextInput`, so a space typed into it is a space and
/// not the transport's play/pause. This is the one binding rule that is only
/// observable from outside.
#[test]
fn space_typed_into_the_composer_is_a_space() {
    let mut session = chat::session(Mode::Headless, WINDOW);
    let value = run(
        &mut session,
        &format!(
            r#"
            {}
            app.type({composer}, "a b");
            app.frames(2);
            app.snapshot().findAll({{ role: "input" }}).map((n) => n.label)
        "#,
            chat::open_chat("chat-typing"),
            composer = chat::composer()
        ),
    );
    let labels: Vec<String> = serde_json::from_value(value).expect("labels");
    assert!(labels.contains(&"a b".to_string()), "{labels:?}");
}
