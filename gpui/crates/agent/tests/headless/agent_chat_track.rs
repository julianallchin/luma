//! Editor navigation preserves the conversation and its unsent draft.
#![cfg(feature = "app")]

use super::support;
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn navigation_preserves_chat_until_an_explicit_new_or_history_choice() {
    let mut app = support::Fixture::new(
        "sticky-chat",
        4,
        vec![support::Clip::new("pattern-strobe", "Strobe", 0.5, 2.0).lit()],
    )
    .with_seeded_threads()
    .with_rig()
    .window(1400., 900.)
    .open(Mode::Headless);
    let script = support::script(&format!(
        r#"
        nav.venue({venue:?});
        const header = (label) => until(label, s => s.find({{role:"text",label}}));
        header("Venue agent");
        until("default venue tab", s => s.find({{role:"card",label:"Test Venue Venue"}}));
        app.type(app.snapshot().find({{role:"input",label:{placeholder:?}}}), "keep this draft");
        nav.track({track:?});
        nav.pattern("Strobe");
        app.frames(8, {{waitMs:20}});
        if (!app.snapshot().find({{role:"input",label:"keep this draft"}})) throw new Error("navigation discarded draft");
        header("Venue agent");

        nav.step("new pattern chat", "button", "New chat");
        header("Pattern agent");
        nav.step("track tab", "button", {track:?});
        app.frames(8, {{waitMs:20}});
        header("Pattern agent");

        nav.step("history", "button", "Chat history");
        until("track history from a pattern chat", s => s.findAll({{role:"card"}}).some(n => n.label.startsWith("Seeded question about score ")));
        const saved = app.snapshot().findAll({{role:"card"}}).find(n => n.label.startsWith("Seeded question about score "));
        app.click(saved);
        header("Track agent");
        until("history dismissed", s => !s.find({{role:"input",label:"Search chats…"}}));
        nav.step("pattern tab", "button", "Strobe");
        app.frames(8, {{waitMs:20}});
        header("Track agent");
        if (!app.snapshot().find({{role:"text",label:saved.label}})) throw new Error("selected conversation changed with tab");
        nav.step("close pattern", "button", "Close Strobe");
        nav.step("close track", "button", "Close Aurora");
        until("venue fallback", s => s.find({{role:"card",label:"Test Venue Venue"}}));
        header("Track agent");
    "#,
        venue = support::VENUE_NAME,
        track = support::TRACK_NAME,
        placeholder = luma_chat::composer::PLACEHOLDER
    ));
    let result = app.exec(&script, Duration::from_secs(120));
    assert_eq!(result.error, None, "{}", result.stdout);
}
