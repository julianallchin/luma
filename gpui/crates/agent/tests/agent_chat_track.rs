//! The chat over the track editor, and the walk that gets there.
//!
//! ```sh
//! cargo test -p gpui-agent --all-features --test agent_chat_track
//! ```
//!
//! Separate from `agent_chat` because the two need different libraries and
//! `LUMA_CONFIG_DIR` is process-global: the chat fixture seeds patterns, and
//! the track agent's scope needs a venue, a track and a score, which is what
//! [`support::Fixture`] already builds. Two fixtures in one binary would be one
//! library with both their contents.
//!
//! No model is scripted here. Nothing sends — what is under test is that the
//! key opens a panel and that the panel lands on the *right conversation*, and
//! resolving a thread is a database round trip that wants no model at all.

#![cfg(feature = "app")]

mod support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture, UNTIL};

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

/// One walk, three screens: the key opens the panel on the venue grid where
/// there is nothing to talk about, the panel survives the track list which is
/// equally about nothing, and it becomes the *track* agent the moment a track
/// is open.
///
/// One test rather than three because it is one panel being carried across
/// three screens — that it is the same panel is half of what is being asserted,
/// and three sessions could not tell.
#[test]
fn the_chat_follows_the_screen_onto_the_track_editor() {
    let mut app = Fixture::new(
        "chat-track",
        TRACK_SECONDS,
        vec![Clip::new("pattern-strobe", "Strobe", 0.5, 2.0).lane(0)],
    )
    .open(Mode::Headless);

    // -- the venue grid: nothing to talk about -------------------------------
    let welcome = run(
        &mut app,
        &format!(
            r#"
            {UNTIL}
            until("the venue grid", (s) => s.find({{ role: "card", label: {venue:?} }}));
            app.action("luma::ToggleAgentChat");
            until("the unattached panel", (s) =>
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
            app.click(app.snapshot().find({{ role: "card", label: {venue:?} }}));
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
            app.click(app.snapshot().find({{ role: "row", label: {track:?} }}));
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
}
