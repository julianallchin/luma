//! The chat follows the eye onto the two venue pages, over a library with no
//! tracks in it at all.
//!
//! The property under test is the one a rig-building session depends on: a
//! room is a subject on its own. Opening the stage or the patch points the
//! centre at that venue's conversation, and neither page needs a track, a
//! score, or an authored document to have one.
//!
//! ```sh
//! cargo test -p gpui-agent --test headless agent_chat_venue
//! ```
#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;

fn labels(nodes: &Value, role: &str) -> Vec<String> {
    nodes
        .as_array()
        .expect("the script returned an array of nodes")
        .iter()
        .filter(|node| node["role"] == role)
        .map(|node| node["label"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Wait for the centre to name an agent, and hand back the whole tree.
const AWAIT_HEADER: &str = r#"
    until("the chat header", (s) => {
        const send = s.find({ role: "button", label: "Send" });
        return send !== undefined && send.bounds.width > 0
            && s.findAll({ role: "text" }).some((n) => n.label === "Venue agent");
    }).nodes
"#;

fn run(harness: &mut Harness, script: &str) -> Value {
    let result = harness.exec(&support::script(script), Duration::from_secs(120));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

#[test]
fn the_chat_attaches_to_the_room_on_the_stage_and_patch_pages() {
    let mut harness = support::Fixture::new("agent-chat-venue", 1, vec![])
        .without_track()
        .with_rig()
        .window(1400., 900.)
        .open(Mode::Headless);

    let stage = run(
        &mut harness,
        &format!(
            "nav.stage({venue:?});\n{AWAIT_HEADER}",
            venue = support::VENUE_NAME,
        ),
    );
    let text = labels(&stage, "text");
    assert!(
        text.iter().any(|label| label == "Venue agent"),
        "the stage page did not point the centre at the room: {text:?}"
    );
    assert!(
        !text
            .iter()
            .any(|label| label == luma_chat::UNATTACHED_BLURB),
        "the chat stayed unattached over a venue: {text:?}"
    );

    // The patch is the same subject, so the same conversation stays up — it is
    // the venue that names it, not the page. Opened by hand rather than
    // through `nav.patch`, which starts at the venue picker and that picker is
    // gone once a venue is selected.
    let patch = run(
        &mut harness,
        &format!(
            r#"
            app.action("luma::NewTab");
            nav.step("the patch choice", "button", "Patch");
            until("the patch tab", (s) =>
                s.find({{ role: "card", label: {tab:?} }}) !== undefined);
            {AWAIT_HEADER}
        "#,
            tab = format!("{} Patch", support::VENUE_NAME),
        ),
    );
    let text = labels(&patch, "text");
    assert!(
        text.iter().any(|label| label == "Venue agent"),
        "the patch page did not point the centre at the room: {text:?}"
    );
}
