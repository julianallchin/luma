//! Reference captures of the status bar's sync item in each of its four
//! states, and the refused changes' list it opens. A person compares the
//! pictures; what is asserted is that each capture shows what it names.
//!
//! Keep them somewhere to look at:
//!
//! ```text
//! LUMA_SHOTS="$PWD/experiments/shots" CARGO_TARGET_DIR="$PIXEL_TARGET" \
//!     cargo test -p gpui-agent --features pixel --test app_pixel sync_status
//! ```

use super::support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_agent::Mode;
use luma_lib::models::sync::{SyncRefusal, SyncStatus};
use serde_json::Value;
use support::Fixture;

fn refusal(id: &str, table: &str, op: &str) -> SyncRefusal {
    SyncRefusal {
        id: id.into(),
        table: table.into(),
        row_id: format!("{table}-1"),
        op: op.into(),
        status: 204,
        at: "2026-11-08T12:00:00Z".into(),
    }
}

#[test]
fn sync_status_captures_each_state() {
    let status = Arc::new(Mutex::new(SyncStatus {
        connected: true,
        ..Default::default()
    }));
    let mut harness = Fixture::new("sync-status-pixels", 4, vec![])
        .with_sync_status(status.clone())
        .open(Mode::Pixel);
    harness.run(
        &support::script(r#"nav.trackEditor("Test Venue", "Aurora"); true;"#),
        Duration::from_secs(120),
    );

    let states: [(&str, &str, SyncStatus); 4] = [
        (
            "up-to-date",
            "Up to date",
            SyncStatus {
                connected: true,
                ..Default::default()
            },
        ),
        (
            "syncing",
            "Syncing",
            SyncStatus {
                connected: true,
                uploading: true,
                pending_uploads: 12,
                ..Default::default()
            },
        ),
        (
            "offline",
            "Offline · 3 pending",
            SyncStatus {
                pending_uploads: 3,
                ..Default::default()
            },
        ),
        (
            "not-saved",
            "2 not saved",
            SyncStatus {
                connected: true,
                refused: vec![
                    refusal("r1", "tracks", "PATCH"),
                    refusal("r2", "fixtures", "DELETE"),
                ],
                ..Default::default()
            },
        ),
    ];
    for (name, label, state) in states {
        *status.lock().unwrap() = state;
        let out: Value = harness.run(
            &support::script(&format!(
                r#"(() => {{
                until({label:?}, (s) => s.find((n) => n.label === {label:?}),
                    {{ timeoutMs: 10000 }});
                app.frames(10, {{ waitMs: 40 }});
                return {{ bar: app.screenshot({{ node: app.snapshot().find({{ role: "card", label: "Status bar" }}) }}) }};
                }})()"#
            )),
            Duration::from_secs(60),
        );
        support::image::keep(&out["bar"], &format!("sync-status/{name}"));
    }

    // The one state that opens: the list of what the server refused.
    let out: Value = harness.run(
        &support::script(
            r#"
            app.click(app.snapshot().find({ role: "button", label: "2 not saved" }));
            until("the refused list", (s) => s.find({ role: "text", label: "Edit to tracks" }));
            app.frames(10, { waitMs: 40 });
            ({ window: app.screenshot() })
            "#,
        ),
        Duration::from_secs(60),
    );
    support::image::keep(&out["window"], "sync-status/not-saved-open");
}
