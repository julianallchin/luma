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

#[path = "../support/chat.rs"]
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

/// The labels of a snapshot array the script already filtered by role.
fn labels_of(nodes: &Value) -> Vec<String> {
    serde_json::from_value(nodes.clone()).expect("an array of labels")
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
        vec!["Running python cell · ramp peak check"],
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
        vec!["Ran python cell · ramp peak check"],
        "the chip did not settle"
    );
    assert!(
        !text.iter().any(|l| l == "Working"),
        "the turn never ended: {text:?}"
    );
}

/// The rendered prose grows while the turn runs. Two snapshots that differ are
/// the only honest proof this is streaming and not one final paint.
///
/// Both samples are taken *inside* the turn, and that is load-bearing:
/// [`chat::send`] does not return until the turn has begun, so `first` is the
/// transcript at the moment streaming started rather than the idle frame. When
/// it was the idle frame this test could pass having watched nothing — the
/// turn-end predicate is also true before the turn starts, so `until` could
/// answer with the same pre-turn snapshot twice and compare it to itself.
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
/// broken everywhere except the graph. The chat is now the shell's centre and
/// is simply *there*, unattached, saying what it could attach to — which is
/// the assertion, because "it exists" and "it explained itself" are different
/// outcomes and only the second one is any use to the person looking at it.
///
/// Both subject-less states the chat fixture can reach, in one session: the
/// cold shell under the venue picker, and the pattern picker. Moving between
/// them keeps the centre, because neither names a subject and there is
/// nothing to re-point.
#[test]
fn the_chat_opens_unattached_on_a_screen_with_no_subject() {
    let mut session = chat::session(Mode::Headless, WINDOW);
    let welcome = run(
        &mut session,
        &format!(
            r#"
            {until}
            until("the unattached centre", (s) => {{
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

    // …and it survives a move to another view that is equally about nothing.
    let patterns = run(
        &mut session,
        r#"
            nav.patterns();
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

/// The centre follows the shell onto its subject rather than vanishing from
/// under it: unattached over the pattern picker, the pattern agent once a
/// pattern's tab is open beside it.
#[test]
fn an_open_panel_re_points_at_the_screen_it_lands_on() {
    let mut session = chat::session(Mode::Headless, WINDOW);
    let attached = run(
        &mut session,
        &format!(
            r#"
            {until}
            until("the unattached centre", (s) =>
                s.findAll({{ role: "text" }}).some((n) => n.label === "Agent"));
            // The pattern door needs a track context now, so the re-point
            // walk goes through a track editor — one more screen for the
            // centre to follow the shell across before it lands on the graph.
            {venue}
            nav.track("Aurora");
            nav.pattern("chat-repoint");
            until("the pattern agent", (s) => {{
                const send = s.find({{ role: "button", label: "Send" }});
                return send && send.bounds.width > 0;
            }}).nodes
        "#,
            until = chat::UNTIL,
            venue = chat::PICK_VENUE,
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

/// The two ways out of the conversation you are in.
///
/// `New chat` is asserted end to end — through the real service and a real
/// database — because "always create" is the whole of its contract: routing it
/// through the ordinary resolve would hand back the conversation already on
/// screen, and the button would look like it did nothing. Clearing back to the
/// empty state is what proves a *different* thread got seated.
#[test]
fn the_chat_chrome_offers_history_and_starts_a_new_conversation() {
    let mut session = chat::session(Mode::Headless, WINDOW);
    let result = run(
        &mut session,
        &format!(
            r#"
            {open}
            const prose = () =>
                app.snapshot().findAll({{ role: "text" }}).map((n) => n.label);
            const chrome =
                app.snapshot().findAll({{ role: "button" }}).map((n) => n.label);
            {send}
            until("the turn end",
                (s) => !s.findAll({{ role: "text" }}).some((n) => n.label === "Working"));
            const settled = prose();
            app.click(app.snapshot().find({{ role: "button", label: "New chat" }}));
            // Polled, not counted: a fixed frame budget is a wall-clock bet,
            // and under a loaded machine it is one this suite loses.
            until("the fresh conversation", (s) =>
                s.findAll({{ role: "text" }})
                    .some((n) => n.label.includes("Where do you want to start?")));
            ({{ chrome, settled, fresh: prose() }})
        "#,
            open = chat::open_chat("chat-new"),
            send = chat::send(),
        ),
    );
    let chrome = labels_of(&result["chrome"]);
    for control in ["Chat history", "New chat"] {
        assert!(
            chrome.iter().any(|label| label == control),
            "{control} is missing from the chat chrome: {chrome:?}"
        );
    }

    let settled = labels_of(&result["settled"]);
    assert!(
        settled
            .iter()
            .any(|l| l.contains("where does the ramp peak?")),
        "the turn never landed: {settled:?}"
    );

    // A new conversation is an empty one: the prompt and the reply are gone,
    // and the opening is back.
    let fresh = labels_of(&result["fresh"]);
    assert!(
        !fresh
            .iter()
            .any(|l| l.contains("where does the ramp peak?")),
        "the previous conversation is still on screen: {fresh:?}"
    );
    assert!(
        fresh
            .iter()
            .any(|l| l.contains("Where do you want to start?")),
        "a new chat did not open on its empty state: {fresh:?}"
    );
}

/// The history picker, end to end: open it, search it, pick a row, and land in
/// the conversation that row named.
///
/// The pick is asserted by *content*, not by a thread id, because the id is
/// what the bug would get right while the reader still ended up somewhere else:
/// `resolve_thread` is newest-wins per subject, so a picker routed through it
/// opens whichever conversation about that track is newest. Seeing the older
/// chat's own prompt come back is what proves the id was pinned.
#[test]
fn the_history_picker_reopens_the_conversation_that_was_picked() {
    let mut session = chat::session(Mode::Headless, WINDOW);
    // Through a venue first, as the real app always is: the shell forces a
    // venue at boot, and the history picker searches that room. It has to
    // happen before the pattern nav, which leaves the venue picker behind.
    run(
        &mut session,
        &format!(
            "{until}\n{venue}",
            until = chat::UNTIL,
            venue = chat::PICK_VENUE
        ),
    );
    let result = run(
        &mut session,
        &format!(
            r#"
            {open}
            const prose = () =>
                app.snapshot().findAll({{ role: "text" }}).map((n) => n.label);
            // Only the dialog's own rows. `card` is a shared role — the tab
            // strip wears one too, and clicking that starts a window drag —
            // as do the shell's regions and the dialog's own frame.
            const chrome = new Set(["Tab strip", "Sidebar", "Stage", "Chat history dialog"]);
            const rows = () => app.snapshot()
                .findAll({{ role: "card" }})
                .filter((n) => !chrome.has(n.label));

            // One conversation with something in it, then a second, empty one.
            {send}
            until("the turn end",
                (s) => !s.findAll({{ role: "text" }}).some((n) => n.label === "Working"));
            const first = prose();
            app.click(app.snapshot().find({{ role: "button", label: "New chat" }}));
            until("the fresh conversation", (s) =>
                s.findAll({{ role: "text" }})
                    .some((n) => n.label.includes("Where do you want to start?")));
            const second = prose();

            // Rewind: the picker lists both, each named by its own words.
            app.click(app.snapshot().find({{ role: "button", label: "Chat history" }}));
            until("the chat list", () => rows().length >= 2);
            const listed = rows().map((n) => n.label);

            // Typing greps the transcripts: the reply's line is a hit, the
            // summaries are not what was searched, and the empty chat has
            // nothing to say.
            // The field is labelled by its value once it has one, so it is
            // re-found by what it currently says.
            const search = (label) => app.snapshot().find({{ role: "input", label }});
            app.type(search("Search chats…"), "downbeat");
            until("the grep hits", () => rows().length >= 1
                && rows().every((n) => n.label.toLowerCase().includes("downbeat")));
            const hits = rows().map((n) => n.label);
            app.type(search("downbeat"), " nothing-says-this");
            until("no matches", (s) =>
                s.find({{ role: "text", label: "No matches" }}) !== undefined);
            app.key("cmd-a");
            app.key("backspace");
            until("the list again", () => rows().length >= 2);

            // Pick the older conversation and land back in it. Newest first,
            // so the one with the turn in it is last.
            app.click(rows().at(-1));
            until("the reopened conversation", (s) =>
                s.findAll({{ role: "text" }})
                    .some((n) => n.label.includes("where does the ramp peak?")));
            ({{ first, second, listed, hits, reopened: prose() }})
        "#,
            open = chat::open_chat("chat-history"),
            send = chat::send(),
        ),
    );

    let first = labels_of(&result["first"]);
    assert!(
        first
            .iter()
            .any(|l| l.contains("where does the ramp peak?")),
        "the first conversation never happened: {first:?}"
    );
    let second = labels_of(&result["second"]);
    assert!(
        !second
            .iter()
            .any(|l| l.contains("where does the ramp peak?")),
        "the new chat still shows the old one: {second:?}"
    );

    let listed = labels_of(&result["listed"]);
    assert!(
        listed.len() >= 2,
        "the picker did not list both conversations: {listed:?}"
    );
    // Rows are named by what was asked in them, and an unspoken one by the
    // placeholder rather than by nothing.
    assert!(
        listed.iter().any(|l| l == "where does the ramp peak?"),
        "the conversation is not named by its opening: {listed:?}"
    );
    assert!(
        listed.iter().any(|l| l == "New chat"),
        "the empty conversation has no name: {listed:?}"
    );
    let hits = labels_of(&result["hits"]);
    assert_eq!(
        hits.len(),
        1,
        "one line of one conversation says 'downbeat': {hits:?}"
    );
    assert!(
        hits[0].contains("Chasing the **downbeat**"),
        "the hit is not the reply's line: {hits:?}"
    );

    // The older conversation is back, with its prompt and its reply.
    let reopened = labels_of(&result["reopened"]);
    assert!(
        reopened
            .iter()
            .any(|l| l.contains("where does the ramp peak?")),
        "picking a row did not reopen that conversation: {reopened:?}"
    );
    assert!(
        reopened.iter().any(|l| l.contains("Chasing the downbeat")),
        "the reopened conversation lost its reply: {reopened:?}"
    );
}
