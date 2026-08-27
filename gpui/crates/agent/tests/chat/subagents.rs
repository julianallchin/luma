//! Delegation, as the three surfaces a reader actually touches.
//!
//! The turn is real below the model: the parent's `subagent` call is the
//! shipped tool, the child is a real `agent_threads` row running a real turn on
//! a private workspace, and the merge really happens. Only the model is
//! scripted — see [`chat::delegating_session`].
//!
//! What is asserted, in the order a person meets it:
//!
//! 1. the transcript chip, while the call has no output — a pill, not the
//!    generic 38px chevron row;
//! 2. the floating pill over the composer, counting live snapshots;
//! 3. the dialog it opens, listing that child;
//! 4. the child's own transcript, morphed into the same card and read-only —
//!    no composer, so there is nothing to send into a conversation that is not
//!    yours;
//! 5. the same chip once the report lands, now reading finished.
//!
//! The dialog's **keyboard** is covered next door, in [`dialog_keyboard`]:
//! this walk drives the controls, so that one drives the keys.

use std::time::Duration;

use gpui_agent::Mode;

#[path = "../support/chat.rs"]
mod chat;

/// Wide enough that the dialog is not clipped by the window, and tall enough
/// that the read-only transcript has rows to show.
const WINDOW: (f32, f32) = (1280., 900.);

#[test]
fn a_delegation_reads_as_a_pill_a_count_and_a_read_only_thread() {
    let mut session = chat::delegating_session(Mode::Headless, WINDOW);
    let result = session.app.exec(
        &format!(
            r#"
            {until}
            {open}
            {send}

            // The chip is a pill: the identicon, the model's own label, and
            // React's own trailing line. The generic chip would read
            // "Started subagent · …" in one node — this reads as two.
            until("the delegation chip", (s) => chips(s).some(
                (c) => c.indexOf({description:?}) >= 0));
            const working_chip = chips(app.snapshot());
            const working_line = app.snapshot().findAll({{ role: "text" }})
                .some((n) => n.label === "started working");

            // The floating pill. Live state: it exists only while a child is
            // in flight, which is what makes waiting on it meaningful.
            until("the subagent pill", (s) =>
                s.find({{ role: "button", label: "1 subagent working" }}) !== undefined);
            const pill_up = true;

            app.click(app.snapshot().find({{ role: "button", label: "1 subagent working" }}));
            until("the subagents dialog", (s) =>
                s.findAll({{ role: "card" }}).some((n) => n.label.indexOf({description:?}) >= 0));
            const listed = app.snapshot().findAll({{ role: "card" }})
                .filter((n) => n.label.indexOf({description:?}) >= 0)
                .map((n) => n.label);

            // Let the delegation finish before reading the child: the reader
            // seats the transcript the thread has when it opens, and a child
            // mid-turn has not written its answer yet.
            until("the turn end", (s) =>
                !s.findAll({{ role: "text" }}).some((n) => n.label === "Working"
                    || n.label === "Sending"));

            // The shell's own composer is behind the scrim and still in the
            // tree, so "no composer" is a claim about what the *reader* adds:
            // count Send before the child is opened and again after.
            const sends_outside = app.snapshot()
                .findAll({{ role: "button", label: "Send" }}).length;

            // By label: the first `card` in the tree is the sidebar, and
            // clicking that dismisses the dialog through the scrim.
            app.click(app.snapshot().findAll({{ role: "card" }})
                .find((n) => n.label.indexOf({description:?}) >= 0));
            until("the child's transcript", (s) =>
                s.findAll({{ role: "text" }}).some((n) => n.label.indexOf({answer:?}) >= 0));
            const inside = app.snapshot();
            const sends = inside.findAll({{ role: "button", label: "Send" }}).length;
            const child_chips = inside.findAll({{ role: "chip" }}).map((n) => n.label);

            // Back to the list, then closed — by the controls. Escape's own
            // two steps are `dialog_keyboard`'s claim, not this one's.
            app.click(app.snapshot().find({{ role: "button", label: "Back" }}));
            app.frames(8, {{ waitMs: 30 }});
            const after_back = app.snapshot().findAll({{ role: "button" }})
                .map((n) => n.label);
            app.click(app.snapshot().find({{ role: "button", label: "Close" }}));
            app.frames(10, {{ waitMs: 30 }});
            const dialog_open = app.snapshot().findAll({{ role: "button", label: "Close" }})
                .length > 0;
            const finished_chip = chips(app.snapshot());
            const finished_line = app.snapshot().findAll({{ role: "text" }})
                .some((n) => n.label === "finished working");

            ({{ working_chip, working_line, pill_up, listed, sends, sends_outside,
                child_chips, after_back, dialog_open,
                finished_chip, finished_line }})
        "#,
            until = chat::UNTIL,
            open = chat::open_chat("chat-subagent"),
            send = chat::send(),
            description = chat::SUBAGENT_DESCRIPTION,
            answer = chat::SUBAGENT_ANSWER,
        ),
        Duration::from_secs(240),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);

    let strings = |key: &str| -> Vec<String> {
        serde_json::from_value(result.result[key].clone()).unwrap_or_default()
    };

    // While the call has no output the chip narrates in the present tense, and
    // the loop never fabricates a finish call — one chip, two readings.
    let working = strings("working_chip");
    assert!(
        working
            .iter()
            .any(|chip| chip.starts_with("Started subagent")),
        "the running delegation must read as a started subagent: {working:?}"
    );
    assert_eq!(
        result.result["working_line"], true,
        "the pill's trailing line must be React's own `started working`"
    );
    assert_eq!(result.result["pill_up"], true);

    let listed = strings("listed");
    assert_eq!(
        listed.len(),
        1,
        "one delegation is one dialog row: {listed:?}"
    );
    assert!(
        listed[0].contains(chat::SUBAGENT_DESCRIPTION),
        "a row is named by the label the model wrote: {listed:?}"
    );

    assert_eq!(
        result.result["sends"], result.result["sends_outside"],
        "the read-only thread must add no composer of its own"
    );
    // The child ran a tool of its own, and its chips render through the same
    // rail the parent's do — which is the whole claim of "no second renderer".
    let child_chips = strings("child_chips");
    assert!(
        child_chips
            .iter()
            .any(|chip| chip.starts_with("Ran python")),
        "the child's own transcript is missing its tool call: {child_chips:?}"
    );

    // Back lands on the list rather than closing: the shell's own Back button
    // survives, the dialog's does not.
    let after = strings("after_back");
    assert_eq!(
        after.iter().filter(|label| *label == "Back").count(),
        1,
        "back must land on the list, not close the dialog: {after:?}"
    );
    assert_eq!(
        result.result["dialog_open"], false,
        "Close must close the dialog"
    );

    let finished = strings("finished_chip");
    assert!(
        finished
            .iter()
            .any(|chip| chip.starts_with("Finished subagent")),
        "the settled delegation must read as a finished subagent: {finished:?}"
    );
    assert_eq!(
        result.result["finished_line"], true,
        "a settled delegation reads `finished working`"
    );
}
