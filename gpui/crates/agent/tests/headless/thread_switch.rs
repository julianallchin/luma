//! Switching score does not flash "no conversation yet" over a conversation
//! that has one.
//!
//! ```sh
//! cargo test -p gpui-agent --test headless thread_switch
//! ```
//!
//! The chat's scope names the open score, so moving the timeline onto another
//! score re-points the centre — and the conversation it lands on arrives from
//! a database read the panel cannot do synchronously. The bug this pins is
//! what the panel did in between: an empty transcript was painted as the
//! *empty state*, so a thread with a history opened on "where do you want to
//! start?" for a frame.
//!
//! The claim is about frames rather than about the end state, and the frames
//! it is about are the ones a settled command draws *through* — the switch
//! draws, the read lands, it draws again, and only the last of those survives
//! into `snapshot()`. So this sweeps `app.painted()`, which is every frame the
//! registry still holds.

#![cfg(feature = "app")]

use super::support;

use std::time::Duration;

use gpui_agent::Mode;
use support::{Clip, Fixture};

const TRACK_SECONDS: u32 = 4;

/// One further score, so there is somewhere to switch to.
const EXTRA_SCORES: usize = 1;

#[test]
fn switching_score_never_flashes_the_empty_thread() {
    let mut app = Fixture::new(
        "thread-switch",
        TRACK_SECONDS,
        vec![Clip::new("pat-glow", "Glow", 1.0, 2.0).lit()],
    )
    .with_extra_scores(EXTRA_SCORES)
    .with_seeded_threads()
    .open(Mode::Headless);

    let script = support::script(&format!(
        r##"
        nav.trackEditor({venue:?}, {track:?});
        const ordinal = () =>
            app.snapshot().findAll({{ role: "text" }})
                .find((n) => n.label.startsWith("SCORE #"))?.label;
        // Which conversation a frame is showing, by the words only its own
        // thread has in it.
        const said = (s) =>
            s.findAll({{ role: "text" }})
                .find((n) => n.label.startsWith({seeded:?}))?.label;
        until("the first score's conversation", (s) => said(s) !== undefined);
        const first = said(app.snapshot());
        const opened = ordinal();

        // Onto the other score, from the sidebar's scores level.
        nav.scores({track:?});
        const rows = () =>
            app.snapshot().findAll({{ role: "row" }}).filter((n) => n.label.startsWith("#"));
        const other = rows().find((n) => !n.label.startsWith(opened.slice("SCORE ".length) + " "));

        // Every frame drawn after the one the row was found in, each read
        // once: `painted` hands back an overlapping window each time, so the
        // high-water mark is what makes the sweep a sweep, not a recount.
        let seen = other.frame;
        let flashed = null;
        let second = null;
        let swept = 0;
        const sweep = () => {{
            for (const shot of app.painted()) {{
                if (shot.frame <= seen) continue;
                seen = shot.frame;
                swept += 1;
                if (flashed === null
                    && shot.find({{ role: "text", label: {headline:?} }}) !== undefined) {{
                    flashed = shot.frame;
                }}
                const words = said(shot);
                if (second === null && words !== undefined && words !== first) {{
                    second = words;
                }}
            }}
        }};

        app.click(other);
        sweep();
        for (let round = 0; round < 40 && second === null; round += 1) {{
            app.frames(1, {{ waitMs: 5 }});
            sweep();
        }}

        ({{ first, second, flashed, swept }})
        "##,
        venue = support::VENUE_NAME,
        track = support::TRACK_NAME,
        seeded = "Seeded question about score ",
        headline = luma_chat::OPENING_HEADLINE,
    ));

    let result = app.exec(&script, Duration::from_secs(180));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out = result.result;

    assert!(
        out["second"].is_string(),
        "the other score's conversation never arrived over {} frames: {out}",
        out["swept"]
    );
    assert_ne!(
        out["first"], out["second"],
        "the switch did not re-point the chat: {out}"
    );
    assert!(
        out["flashed"].is_null(),
        "the empty state was painted over a thread that had history, \
         on frame {}: {out}",
        out["flashed"]
    );
}
