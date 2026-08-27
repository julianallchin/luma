//! Escape closes every dialog, including the ones that move the keyboard to a
//! control of their own after opening.
//!
//! The dialogs differ in *who holds focus a frame after they open*: the pattern
//! picker leaves it on the host's trap handle, while `AddTracks` and the chat
//! history picker route it onward to a text field. Both are inside the trap and
//! both are under the overlay's `key_context`, so `escape` has to resolve the
//! same way — but only one of those shapes was covered, and "the dialog took
//! the keyboard" and "the dialog can still be dismissed by it" are different
//! claims.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::Mode;
use serde_json::Value;

#[test]
fn escape_dismisses_a_dialog_that_routed_focus_to_its_own_text_field() {
    let mut harness = support::Fixture::new("dialog-escape", 1, vec![]).open(Mode::Headless);
    let script = support::script(
        r#"
        nav.venue("Test Venue");
        nav.step("the add-track affordance", "button", "Add track");
        // The dialog's own field takes the keyboard from the host's trap
        // handle: this is the shape the pattern picker does not have.
        const opened = until("the add-tracks browser", (s) =>
            s.find({ role: "input", label: "Search all tracks…" })?.focused === true ? s : undefined);
        const cardFocused = opened.find({ role: "card", label: "Add tracks dialog" })?.focused;

        app.key("escape");
        app.frames(4);
        const after = app.snapshot();
        ({
            cardFocused,
            dismissed: after.find({ role: "card", label: "Add tracks dialog" }) === undefined,
        })
        "#,
    );
    let result = harness.exec(&script, Duration::from_secs(120));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    assert_eq!(
        out["cardFocused"], true,
        "the dialog's field took the keyboard outside the host's trap: {out:#}"
    );
    assert_eq!(
        out["dismissed"], true,
        "escape did not close the dialog: {out:#}"
    );
}
