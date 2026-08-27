//! The keyboard of the two dialogs the thread opens.
//!
//! Every other dialog in the app is reached from the shell, and their Escape is
//! covered by the headless suite. These two are reached from the chat panel —
//! whose composer is a focused text field at the moment the dialog opens — so
//! they exercise the one case the shell's dialogs cannot: a dismissal that has
//! to out-rank the field the keyboard was in. `Luma::dismiss_overlay` is the
//! only handler either can reach, and it is reachable only while the focus path
//! runs through the overlay's own key context.

use std::time::Duration;

use gpui_agent::Mode;

#[path = "../support/chat.rs"]
mod chat;

const WINDOW: (f32, f32) = (1280., 900.);

#[test]
fn escape_closes_the_history_picker_the_thread_opened() {
    let mut session = chat::session(Mode::Headless, WINDOW);
    let result = session.app.exec(
        &format!(
            r#"
            {open}
            // The composer holds the keyboard before the dialog exists: that
            // is the state a shell-opened dialog never starts from.
            const composerFocused = {composer}?.focused === true;
            app.click(app.snapshot().find({{ role: "button", label: "Chat history" }}));
            const up = until("the history picker", (s) =>
                s.find({{ role: "card", label: "Chat history dialog" }}) ? s : undefined);
            const cardFocused = up.find({{ role: "card", label: "Chat history dialog" }}).focused;
            const focusedNodes = up.findAll((n) => n.focused).map((n) => `${{n.role}}:${{n.label}}`);

            app.key("escape");
            app.frames(6, {{ waitMs: 20 }});
            const after = app.snapshot();
            ({{
                composerFocused,
                cardFocused,
                focusedNodes,
                dismissed: after.find({{ role: "card", label: "Chat history dialog" }}) === undefined,
            }})
        "#,
            open = chat::open_chat("chat-history"),
            composer = chat::composer(),
        ),
        Duration::from_secs(180),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out = &result.result;
    assert_eq!(
        out["cardFocused"], true,
        "the picker never took the keyboard: {out:#}"
    );
    assert_eq!(
        out["dismissed"], true,
        "escape did not close the picker: {out:#}"
    );
}

/// The subagents dialog, whose keyboard is its own: Escape steps a child's
/// transcript back to the list before it closes the list, and the arrows walk
/// the rows. Both go through `Luma::subagents_key`, which only runs while the
/// focus path is inside the card — so this pins the routing as much as the
/// behaviour.
#[test]
fn escape_steps_the_subagents_dialog_back_before_it_closes_it() {
    let mut session = chat::delegating_session(Mode::Headless, WINDOW);
    let result = session.app.exec(
        &format!(
            r#"
            {until}
            {open}
            {send}
            until("the subagent pill", (s) =>
                s.find({{ role: "button", label: "1 subagent working" }}) !== undefined);
            app.click(app.snapshot().find({{ role: "button", label: "1 subagent working" }}));
            const list = until("the subagents dialog", (s) =>
                s.findAll({{ role: "card" }}).some((n) => n.label.indexOf({description:?}) >= 0)
                    ? s : undefined);
            const rowFocused = list.findAll({{ role: "card" }})
                .some((n) => n.label.indexOf({description:?}) >= 0 && n.focused);
            until("the turn end", (s) =>
                !s.findAll({{ role: "text" }}).some((n) => n.label === "Working"
                    || n.label === "Sending"));

            // Right opens the focused row — the same gesture Enter is bound to.
            app.key("right");
            until("the child's transcript", (s) =>
                s.findAll({{ role: "text" }}).some((n) => n.label.indexOf({answer:?}) >= 0));
            const opened = true;
            // The route change unmounts the row the keyboard was on; the shell
            // has to notice and re-seat it, or nothing below reaches a handler.
            const threadSeated = app.snapshot()
                .find({{ role: "card", label: "Subagents dialog" }})?.focused === true;

            // Innermost first: the transcript steps back to the list…
            app.key("escape");
            const back = until("the list again", (s) =>
                s.findAll({{ role: "card" }}).some((n) => n.label.indexOf({description:?}) >= 0)
                    ? s : undefined);
            const stillOpen = back.find({{ role: "button", label: "Close" }}) !== undefined;

            // …and only then does the list itself close.
            app.key("escape");
            app.frames(8, {{ waitMs: 20 }});
            const closed = app.snapshot().find({{ role: "button", label: "Close" }}) === undefined;
            ({{ rowFocused, opened, threadSeated, stillOpen, closed }})
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
    let out = &result.result;
    assert_eq!(
        out["rowFocused"], true,
        "the dialog did not seat the keyboard on its first row: {out:#}"
    );
    assert_eq!(out["opened"], true);
    assert_eq!(
        out["threadSeated"], true,
        "the keyboard was left on the row the route change unmounted: {out:#}"
    );
    assert_eq!(
        out["stillOpen"], true,
        "escape closed the whole dialog instead of stepping back: {out:#}"
    );
    assert_eq!(
        out["closed"], true,
        "escape did not close the list: {out:#}"
    );
}
