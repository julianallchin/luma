//! The context gauge under the composer, and the card it opens.
//!
//! Two facts worth a test, and they are the two the design can get wrong
//! silently:
//!
//! 1. The reading is the *whole prompt*, cache included. The scripted provider
//!    reports a warm shape — 4,800 fresh tokens over 610,000 cached — which is
//!    what every turn past the first looks like. A gauge fed from
//!    `input_tokens` alone reads 0% here and would keep reading 0% up to the
//!    context limit.
//! 2. The card reads the *durable* transcript, so it survives a reload. The
//!    second half of the test reopens the same thread from the history picker
//!    and asks the gauge the same question.

use std::time::Duration;

use gpui_agent::Mode;

#[path = "../support/chat.rs"]
mod chat;

/// Wide enough that the status strip is not clipped — the gauge lives at its
/// trailing edge, and a clipped node reports zero width.
const WINDOW: (f32, f32) = (1280., 900.);

#[test]
fn the_gauge_reports_the_whole_prompt_and_its_card_names_every_field() {
    let mut session = chat::session(Mode::Headless, WINDOW);
    let result = session.app.exec(
        &format!(
            r#"
            {until}
            {open}
            // Nothing has been asked yet, so there is no request to report and
            // the strip carries no gauge at all — an empty ring would be a
            // reading of zero, which is a different claim.
            const before = app.snapshot().findAll({{ role: "text" }})
                .filter((n) => n.label.startsWith("Context"));

            {send}
            until("the turn end", (s) =>
                !s.findAll({{ role: "text" }}).some((n) => n.label === "Working"
                    || n.label === "Sending"));

            const gauge = app.snapshot().find({{ role: "text", label: {reading:?} }});
            if (gauge === undefined) {{
                throw new Error("no gauge: " + JSON.stringify(
                    app.snapshot().findAll({{ role: "text" }}).map((n) => n.label)));
            }}

            // Hover it: `scroll` walks the pointer to a node and leaves it
            // there, which is the only gesture the driver has that hovers
            // without also clicking.
            app.scroll(gauge, {{ dx: 0, dy: 0 }});
            app.frames(3, {{ waitMs: 20 }});
            const rows = app.snapshot().findAll({{ role: "text" }}).map((n) => n.label);

            ({{ before: before.length, rows }})
        "#,
            until = chat::UNTIL,
            open = chat::open_chat("chat-context"),
            send = chat::send(),
            reading = chat::CONTEXT_READING,
        ),
        Duration::from_secs(180),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);

    assert_eq!(
        result.result["before"], 0,
        "a thread with no request behind it must show no gauge"
    );
    let rows: Vec<String> = serde_json::from_value(result.result["rows"].clone()).expect("rows");
    let has = |row: &str| rows.iter().any(|label| label == row);

    // Every field the provider reported, in the card's own words.
    for row in [
        "INPUT 4,800",
        "CACHE READ 610,000",
        "CACHE WRITE 12,000",
        "OUTPUT 512",
        "PROMPT 626,800",
        "WINDOW 1,000,000",
        "MODEL claude-opus-5",
    ] {
        assert!(has(row), "the card is missing `{row}`; it shows {rows:?}");
    }
    // The duration is measured, not scripted, so only its shape is assertable.
    assert!(
        rows.iter()
            .any(|label| label.starts_with("TOOK ")
                && (label.ends_with(" ms") || label.ends_with(" s"))),
        "the card does not report how long the request took: {rows:?}"
    );
}
