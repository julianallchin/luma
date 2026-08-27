//! The chat over the track editor, and the walk that gets there.
//!
//! ```sh
//! cargo test -p gpui-agent --all-features --test agent_chat_track
//! ```
//!
//! Separate from `agent_chat` because the two need different libraries: the
//! chat fixture seeds patterns, and the track agent's scope needs a venue, a
//! track and a score, which is what [`support::Fixture`] already builds.
//!
//! They are separate *tests*, not separate processes. Each fixture gets its own
//! directory and carries it in the pump thread's [`luma_ui::runtime::Runtime`],
//! so two libraries coexist here fine. What does not coexist is anything the
//! Luma lib caches process-wide on a key the fixtures share — see
//! [`support::Fixture::track_hash`], which is where that already went wrong.
//!
//! No model is scripted here. Nothing sends — what is under test is that the
//! key opens a panel and that the panel lands on the *right conversation*, and
//! resolving a thread is a database round trip that wants no model at all.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture, NAV};

/// The shortest track the editor will open: it decodes the file on the way in,
/// and nothing here reads the waveform.
const TRACK_SECONDS: u32 = 4;

fn run(app: &mut Harness, code: &str) -> Value {
    let result = app.exec(code, Duration::from_secs(120));
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

/// One walk, three states of the shell: the centre sits unattached under the
/// venue picker where there is nothing to talk about, survives venue selection
/// which is equally about nothing, and becomes the *track* agent the moment a
/// track's tab is open beside it.
///
/// One test rather than three because it is one centre being carried across
/// four states — including a subject-less visualizer after attachment. That
/// it is the same centre is half of what is being asserted, and separate
/// sessions could not tell.
#[test]
fn the_chat_follows_the_screen_onto_the_track_editor() {
    let mut app = Fixture::new(
        "chat-track",
        TRACK_SECONDS,
        vec![Clip::new("pattern-strobe", "Strobe", 0.5, 2.0)
            .lane(0)
            .lit()],
    )
    .open(Mode::Headless);

    // -- the venue grid: nothing to talk about -------------------------------
    let welcome = run(
        &mut app,
        &format!(
            r#"
            {NAV}
            until("the venue picker", (s) => s.find({{ role: "card", label: {venue:?} }}));
            until("the unattached centre", (s) =>
                s.find({{ role: "text", label: {blurb:?} }}));
            app.frames(8, {{ waitMs: 40 }});
            app.snapshot().nodes
        "#,
            venue = support::VENUE_NAME,
            blurb = luma_chat::UNATTACHED_BLURB,
        ),
    );
    let text = labels(&welcome, "text");
    assert!(
        text.iter().any(|l| l == "Agent"),
        "the key did nothing on the venue grid: {text:?}"
    );

    // -- the track list: still nothing ---------------------------------------
    let tracks = run(
        &mut app,
        &format!(
            r#"
            nav.venue({venue:?});
            until("the track list", (s) => s.find({{ role: "row", label: {track:?} }}));
            app.frames(4, {{ waitMs: 20 }});
            app.snapshot().nodes
        "#,
            venue = support::VENUE_NAME,
            track = support::TRACK_NAME,
        ),
    );
    assert!(
        labels(&tracks, "text").iter().any(|l| l == "Agent"),
        "the panel was dropped on the way into the venue: {:?}",
        labels(&tracks, "text")
    );

    // -- the track editor: the track agent -----------------------------------
    let editor = run(
        &mut app,
        &format!(
            r#"
            nav.track({track:?});
            until("the track agent", (s) => {{
                const send = s.find({{ role: "button", label: "Send" }});
                return send !== undefined && send.bounds.width > 0
                    && s.findAll({{ role: "text" }}).some((n) => n.label === "Track agent");
            }}).nodes
        "#,
            track = support::TRACK_NAME,
        ),
    );
    let text = labels(&editor, "text");
    assert!(
        text.iter().any(|l| l == "Track agent"),
        "the panel did not re-point onto the track's scope: {text:?}"
    );
    assert!(
        !text.iter().any(|l| l == "Agent"),
        "the unattached header is still up beside the attached one: {text:?}"
    );
    assert!(
        !text.iter().any(|l| l == luma_chat::UNATTACHED_BLURB),
        "the attached panel is still offering the unattached copy: {text:?}"
    );

    // -- a subject-less tab: keep the attached track conversation -----------
    // The patch is the subject-less tab now that the stage is not a tab at
    // all: it names a room, and a room is not something an agent works on.
    let universe = run(
        &mut app,
        r#"
            nav.step("the add-tab control", "button", "new-tab");
            nav.step("the universe choice", "button", "Universe setup");
            until("the patch tab", (s) =>
                s.findAll({ role: "text" }).some((n) => n.label === "Track agent"))
                .nodes
        "#,
    );
    let text = labels(&universe, "text");
    assert!(
        text.iter().any(|l| l == "Track agent"),
        "the subject-less patch detached the track conversation: {text:?}"
    );
    assert!(
        !text.iter().any(|l| l == luma_chat::UNATTACHED_BLURB),
        "the patch replaced the attached conversation with an unattached one: {text:?}"
    );

    // -- a pattern tab: the pattern's conversation ----------------------------
    // Each editor gets its companion agent, keyed on the tab in front — the
    // web app pairs the pattern editor with the graph agent. Only tabs with
    // no agent of their own (the patch, above) fall back to the track.
    let graph = run(
        &mut app,
        &format!(
            r#"
            nav.pattern({pattern:?});
            until("the pattern agent", (s) =>
                s.findAll({{ role: "text" }}).some((n) => n.label === "Pattern agent"));
            app.frames(6, {{ waitMs: 30 }});
            app.snapshot().nodes
        "#,
            pattern = "Strobe",
        ),
    );
    let text = labels(&graph, "text");
    assert!(
        text.iter().any(|l| l == "Pattern agent"),
        "the graph tab did not hand the panel to the pattern agent: {text:?}"
    );
    assert!(
        !text.iter().any(|l| l == "Track agent"),
        "the track header is still up beside the pattern agent's: {text:?}"
    );

    // -- and back: the front tab decides --------------------------------------
    let returned = run(
        &mut app,
        r#"
            nav.step("the track tab", "button", "Aurora");
            until("the track agent again", (s) =>
                s.findAll({ role: "text" }).some((n) => n.label === "Track agent"))
                .nodes
        "#,
    );
    let text = labels(&returned, "text");
    assert!(
        text.iter().any(|l| l == "Track agent"),
        "returning to the track tab did not bring its conversation back: {text:?}"
    );

    // -- the captured context stands alone ------------------------------------
    // A graph tab resolves its track context at open and then owns it; the
    // track editor is not a live dependency. Closing it heals the selection
    // onto the graph tab, which keeps its canvas, its context readout and its
    // conversation.
    let standalone = run(
        &mut app,
        r#"
            nav.step("the track tab's close affordance", "button", "Close Aurora");
            // The close heals onto a neighbour; the assertion is about the
            // graph tab, so put it in front.
            nav.step("the graph tab", "button", "Strobe");
            until("the graph tab standing alone", (s) =>
                s.findAll({ role: "text" }).some((n) => n.label === "Pattern agent"))
                .nodes
        "#,
    );
    let text = labels(&standalone, "text");
    assert!(
        text.iter().any(|l| l.ends_with("NODES")),
        "closing the track editor lost the graph canvas: {text:?}"
    );
    assert!(
        text.iter().any(|l| l == "TRACK AURORA"),
        "the captured context did not survive its track editor: {text:?}"
    );
}
