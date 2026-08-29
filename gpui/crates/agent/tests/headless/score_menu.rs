//! Right-clicking a score, and what the menu can do to it.
//!
//! Three claims:
//!
//! - a right-click on a score row raises the one context menu, and its rows
//!   are findable by role and label like every other control — which is the
//!   whole point of putting the primitive in `luma-ui` rather than painting a
//!   menu by hand in the sidebar;
//! - deleting a score that holds clips asks first, quoting the count the row
//!   showed, and the row survives a cancel;
//! - confirming removes it from the list, and — when it was the score on the
//!   timeline — leaves the editor in the defined `NO SCORE` state rather than
//!   drawing a document that no longer exists.
//!
//! The empty case is the fourth: a score with nothing in it goes without a
//! dialog, because a confirmation that can only be answered one way is a
//! confirmation nobody reads.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::Value;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 8;

/// Two further scores, so there is an empty one to delete without a dialog and
/// the list has something left over afterwards.
const EXTRA_SCORES: usize = 2;

fn harness(name: &'static str) -> Harness {
    Fixture::new(
        name,
        TRACK_SECONDS,
        vec![Clip::new("pat-glow", "Glow", 1.0, 4.0)],
    )
    .with_extra_scores(EXTRA_SCORES)
    .open(Mode::Headless)
}

/// The reading every case shares: the level's rows, and which one holds clips.
const READ: &str = r##"
    nav.venue("Test Venue");
    nav.scores("Aurora");

    const rows = () =>
        app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
    const labels = () => rows().map((n) => n.label);
    const withClips = () => rows().find((n) => n.label.includes("· 1 clips ·"));
    const empty = () => rows().find((n) => n.label.includes("· 0 clips ·"));
    const menu = () => app.snapshot().find({ role: "card", label: "Context menu" });
    const dialog = () => app.snapshot().find({ role: "card", label: "Confirm dialog" });
    const deleteItem = () => app.snapshot().find({ role: "button", label: "Delete score" });
"##;

/// The menu opens where it was asked for, and its rows are real nodes.
const OPENS: &str = r##"
    const before = menu() !== undefined;
    const row = withClips();
    app.click(row, { button: "right" });
    const shot = until("the score menu", (s) =>
        s.find({ role: "card", label: "Context menu" }) !== undefined ? s : undefined);
    const card = shot.find({ role: "card", label: "Context menu" });
    const item = shot.find({ role: "button", label: "Delete score" });

    // Escape is the other door out, and it has to lead to the same place.
    app.key("escape");
    app.frames(2);
    const afterEscape = menu() !== undefined;

    ({
        before,
        // A menu nothing can click is a menu that is not there: the harness
        // clips a node's bounds to the content mask, so a card cut off by the
        // sidebar's clip would report an empty box.
        cardArea: card.bounds.width * card.bounds.height,
        itemArea: item.bounds.width * item.bounds.height,
        // It hangs at the pointer, not at the top of the column.
        below: card.bounds.y >= row.bounds.y - 1,
        afterEscape,
    })
"##;

/// A score with clips asks; cancelling keeps it; confirming takes it — and the
/// timeline it was on with it.
const CONFIRMS: &str = r##"
    const listed = labels();
    app.click(withClips(), { button: "right" });
    until("the score menu", () => menu() !== undefined);
    app.click(deleteItem());
    const asked = until("the confirmation", (s) =>
        s.find({ role: "card", label: "Confirm dialog" }) !== undefined ? s : undefined);
    // The sentence quotes what the row said, so the two cannot disagree.
    const prose = asked.findAll({ role: "text" }).map((n) => n.label).join(" | ");

    app.click(asked.find({ role: "button", label: "Cancel" }));
    until("the dialog gone", () => dialog() === undefined);
    const afterCancel = labels();

    // Again, and through this time.
    app.click(withClips(), { button: "right" });
    until("the score menu again", () => menu() !== undefined);
    app.click(deleteItem());
    until("the confirmation again", () => dialog() !== undefined);
    app.click(app.snapshot().find({ role: "card", label: "Confirm dialog" })
        && app.snapshot().findAll({ role: "button" })
            .filter((n) => n.label === "Delete score")
            .at(-1));
    until("the score gone from the list", () => withClips() === undefined && dialog() === undefined);
    const afterDelete = labels();

    ({ listed, prose, afterCancel, afterDelete })
"##;

/// Deleting the score the timeline is on leaves the editor in its defined
/// empty state, and gets there without ever drawing the deleted document.
const EDITOR: &str = r##"
    const ordinal = () =>
        app.snapshot().findAll({ role: "text" }).find((n) => n.label.startsWith("SCORE #"))?.label;
    const noScore = () =>
        app.snapshot().findAll({ role: "text" }).some((n) => n.label === "NO SCORE");

    const before = labels();

    // Choosing a score from this level opens it on the timeline and stays
    // here, which is exactly the state this case needs: the list and the
    // editor looking at the same document.
    app.click(withClips());
    until("the timeline on the chosen score", () => ordinal() !== undefined);
    const open = ordinal();

    // The `#N` handle is a position in the list, not a name: deleting a score
    // renumbers the ones after it, so a survivor inherits the handle and `#N`
    // cannot say *which* score across the deletion. The clip count can — the
    // fixture hangs its one clip on exactly the score being opened here.
    app.click(withClips(), { button: "right" });
    until("the score menu", () => menu() !== undefined);
    app.click(deleteItem());
    // Either door: a score with clips asks, an empty one does not.
    if (dialog() !== undefined) {
        app.click(app.snapshot().findAll({ role: "button" })
            .filter((n) => n.label === "Delete score").at(-1));
    }
    until("the editor off the score", () => noScore());

    ({ open, gone: ordinal() === undefined, before, listed: labels() })
"##;

/// An empty score goes straight away.
const EMPTY: &str = r##"
    const before = labels();
    app.click(empty(), { button: "right" });
    until("the score menu", () => menu() !== undefined);
    app.click(deleteItem());
    until("one fewer score", () => rows().length === before.length - 1);
    const askedAnyway = dialog() !== undefined;
    ({ before, after: labels(), askedAnyway })
"##;

fn run(name: &'static str, script: &str) -> Value {
    let mut harness = harness(name);
    let result = harness.exec(
        &support::script(&format!("{READ}\n{script}")),
        Duration::from_secs(300),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

fn labels(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("no {key} in {value}"))
        .iter()
        .map(|label| label.as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn a_right_click_on_a_score_raises_a_findable_menu_and_escape_takes_it_away() {
    let out = run("score-menu-opens", OPENS);
    assert_eq!(out["before"], false, "a menu was up before anything asked");
    assert!(
        out["cardArea"].as_f64().unwrap_or_default() > 0.0,
        "the menu card has no clickable area — it is clipped by the sidebar: {out}"
    );
    assert!(
        out["itemArea"].as_f64().unwrap_or_default() > 0.0,
        "`Delete score` is registered but unclickable: {out}"
    );
    assert_eq!(out["below"], true, "the menu did not hang at the pointer");
    assert_eq!(
        out["afterEscape"], false,
        "Escape left the menu up — it is not on the dismissal ladder"
    );
}

#[test]
fn deleting_a_score_with_clips_asks_first_and_the_answer_decides() {
    let out = run("score-menu-confirm", CONFIRMS);
    let prose = out["prose"].as_str().unwrap_or_default();
    assert!(
        prose.contains("1 clip"),
        "the confirmation does not quote the row's clip count: {prose}"
    );
    let listed = labels(&out, "listed");
    let cancelled = labels(&out, "afterCancel");
    assert_eq!(
        listed, cancelled,
        "Cancel took the score anyway — the dialog is not the decision"
    );
    let after = labels(&out, "afterDelete");
    assert_eq!(
        after.len(),
        listed.len() - 1,
        "confirming did not remove the row: {listed:?} → {after:?}"
    );
    assert!(
        !after.iter().any(|label| label.contains("· 1 clips ·")),
        "the deleted score is still listed: {after:?}"
    );
}

#[test]
fn deleting_the_open_score_leaves_the_editor_with_no_score() {
    let out = run("score-menu-editor", EDITOR);
    assert_eq!(
        out["gone"], true,
        "the timeline is still naming a score that was deleted: {out}"
    );
    let before = labels(&out, "before");
    let listed = labels(&out, "listed");
    assert_eq!(
        listed.len(),
        before.len() - 1,
        "the list did not lose a row: {before:?} → {listed:?}"
    );
    // Not the `#N` the editor was showing: ordinals are positions and they
    // renumber on delete, so the handle outlives the score that wore it.
    assert!(
        !listed.iter().any(|label| label.contains("· 1 clips ·")),
        "the deleted score ({}) is still on the list: {listed:?}",
        out["open"].as_str().unwrap_or_default()
    );
}

#[test]
fn deleting_an_empty_score_does_not_ask() {
    let out = run("score-menu-empty", EMPTY);
    assert_eq!(
        out["askedAnyway"], false,
        "an empty score raised a confirmation nobody can answer twice"
    );
    let before = labels(&out, "before");
    let after = labels(&out, "after");
    assert_eq!(after.len(), before.len() - 1, "{before:?} → {after:?}");
}
