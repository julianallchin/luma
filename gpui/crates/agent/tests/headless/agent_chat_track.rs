//! A track's chat survives navigation inside the track, and the venue page.
#![cfg(feature = "app")]

use super::support;
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn a_track_keeps_its_chat_until_an_explicit_new_or_history_choice() {
    let mut app = support::Fixture::new(
        "sticky-chat",
        4,
        vec![support::Clip::new("pattern-strobe", "Strobe", 0.5, 2.0)],
    )
    .with_seeded_threads()
    .with_rig()
    .window(1400., 900.)
    .open(Mode::Headless);
    let script = support::script(&format!(
        r#"
        const header = (label) => until(label, s => s.find({{role:"text",label}}));
        nav.trackEditor({venue:?}, {track:?});
        header("Luma");
        until("score clip",s=>s.find({{role:"card",label:"Strobe"}}));
        // Picking the track opened its most recent chat, a seeded one.
        until("the track's chat", s => s.findAll({{role:"text"}}).some(n => n.label.startsWith("Seeded question about score ")));
        app.type(until("composer", s => s.find({{role:"input",label:{placeholder:?}}})).find({{role:"input",label:{placeholder:?}}}), "keep this draft");
        nav.step("new chat", "button", "New chat");
        header("Luma");
        nav.step("history", "button", "Chat history");
        until("track history", s => s.findAll({{role:"card"}}).some(n => n.label.startsWith("Seeded question about score ")));
        const saved = app.snapshot().findAll({{role:"card"}}).find(n => n.label.startsWith("Seeded question about score "));
        app.click(saved);
        header("Luma");
        until("history dismissed", s => !s.find({{role:"input",label:"Search chats…"}}));
        // The venue page hides the thread; coming back to the same track
        // shows the same conversation, not a fresh resolve.
        nav.venuePage({venue:?});
        until("thread hidden", s => !s.findAll({{role:"text"}}).some(n => n.label === "Luma"));
        nav.step("the track again", "row", {track:?});
        header("Luma");
        app.frames(8, {{waitMs:20}});
        if (!app.snapshot().find({{role:"text",label:saved.label}})) throw new Error("the venue page replaced the track's conversation");
        nav.step("close track", "button", "Close Aurora");
        until("closed score workspace", s => !s.find({{role:"button",label:"Aurora"}}));
        header("Luma");
    "#,
        venue = support::VENUE_NAME,
        track = support::TRACK_NAME,
        placeholder = luma_chat::composer::PLACEHOLDER
    ));
    let result = app.exec(&script, Duration::from_secs(120));
    assert_eq!(result.error, None, "{}", result.stdout);
}
