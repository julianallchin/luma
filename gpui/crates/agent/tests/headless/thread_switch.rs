//! Choosing another score changes the editor, never the selected conversation.
#![cfg(feature = "app")]
use super::support;
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn switching_score_preserves_the_selected_thread() {
    let mut app = support::Fixture::new(
        "thread-switch",
        4,
        vec![support::Clip::new("pat-glow", "Glow", 1., 2.).lit()],
    )
    .with_extra_scores(1)
    .with_seeded_threads()
    .open(Mode::Headless);
    let script = support::script(&format!(
        r##"
        nav.trackEditor({venue:?}, {track:?});
        nav.step("history", "button", "Chat history");
        until("seeded conversations", s => s.findAll({{role:"card"}}).filter(n => n.label.startsWith("Seeded question about score ")).length === 2);
        const selected = app.snapshot().findAll({{role:"card"}}).find(n => n.label.startsWith("Seeded question about score "));
        app.click(selected);
        until("selected transcript", s => s.find({{role:"text",label:selected.label}}));
        until("history dismissed", s => !s.find({{role:"input",label:"Search chats…"}}));
        const opened = app.snapshot().findAll({{role:"text"}}).find(n => n.label.startsWith("SCORE #")).label;
        nav.scores({track:?});
        const other = app.snapshot().findAll({{role:"row"}}).find(n => n.label.startsWith("#") && !n.label.startsWith(opened.slice("SCORE ".length)+" "));
        app.click(other);
        app.frames(20, {{waitMs:20}});
        if (!app.snapshot().find({{role:"text",label:selected.label}})) throw new Error("score selection replaced chat");
    "##,
        venue = support::VENUE_NAME,
        track = support::TRACK_NAME
    ));
    let result = app.exec(&script, Duration::from_secs(120));
    assert_eq!(result.error, None, "{}", result.stdout);
}
