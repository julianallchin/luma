//! A chat belongs to its score: choosing another score shows that score's
//! chat, and the history lists only the open score's chats.
#![cfg(feature = "app")]
use super::support;
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn switching_score_switches_to_that_scores_chat() {
    let mut app = support::Fixture::new(
        "thread-switch",
        4,
        vec![support::Clip::new("pat-glow", "Glow", 1., 2.)],
    )
    .with_extra_scores(1)
    .with_seeded_threads()
    .open(Mode::Headless);
    let script = support::script(&format!(
        r##"
        const seeded = (s) => s.findAll({{role:"text"}}).find(n => n.label.startsWith("Seeded question about score "));
        nav.trackEditor({venue:?}, {track:?});
        const first = seeded(until("the open score's chat", s => seeded(s))).label;

        nav.step("history", "button", "Chat history");
        until("this score's chats", s => s.findAll({{role:"card"}}).some(n => n.label.startsWith("Seeded question about score ")));
        const listed = app.snapshot().findAll({{role:"card"}}).filter(n => n.label.startsWith("Seeded question about score ")).length;
        nav.dismiss();
        until("history dismissed", s => !s.find({{role:"input",label:"Search chats…"}}));

        const opened = app.snapshot().findAll({{role:"text"}}).find(n => n.label.startsWith("Score #")).label;
        nav.scores({track:?});
        const other = app.snapshot().findAll({{role:"row"}}).find(n => n.label.startsWith("#") && !n.label.startsWith(opened.slice("SCORE ".length)+" "));
        app.click(other);
        const second = seeded(until("the other score's chat", s => {{
            const text = seeded(s);
            return text && text.label !== first ? s : undefined;
        }})).label;
        ({{ first, second, listed }})
    "##,
        venue = support::VENUE_NAME,
        track = support::TRACK_NAME
    ));
    let result = app.exec(&script, Duration::from_secs(120));
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    assert_ne!(out["first"], out["second"], "{out:#}");
    assert_eq!(
        out["listed"], 1,
        "the history listed another score's chats: {out:#}"
    );
}
