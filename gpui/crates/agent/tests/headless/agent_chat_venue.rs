//! A venue has no chat, over a library with no tracks in it at all.
//!
//! With no score open the thread says how to start one, and the venue page
//! takes the thread's room instead of pointing it at the room.
//!
//! ```sh
//! cargo test -p gpui-agent --test headless agent_chat_venue
//! ```
#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::Mode;

#[test]
fn the_thread_waits_for_a_score_and_the_venue_page_hides_it() {
    let mut harness = support::Fixture::new("agent-chat-venue", 1, vec![])
        .without_track()
        .with_rig()
        .window(1400., 900.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(&format!(
            r#"
            nav.venue({venue:?});
            const idle = until("the unattached thread", (s) =>
                s.find({{ role: "text", label: {headline:?} }}) ? s : undefined);
            nav.venuePage({venue:?});
            const page = until("the thread collapsed", (s) =>
                !s.findAll({{ role: "text" }}).some((n) => n.label === "Luma") ? s : undefined);
            ({{
                idleSend: idle.find({{ role: "button", label: "Send" }}) !== undefined,
                pageHeadline: page.find({{ role: "text", label: {headline:?} }}) !== undefined,
            }})
        "#,
            venue = support::VENUE_NAME,
            headline = luma_chat::UNATTACHED_HEADLINE,
        )),
        Duration::from_secs(120),
    );
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out = result.result;
    assert_eq!(
        out["idleSend"], false,
        "an unattached thread offered a send: {out:#}"
    );
    assert_eq!(
        out["pageHeadline"], false,
        "the venue page left the thread on screen: {out:#}"
    );
}
