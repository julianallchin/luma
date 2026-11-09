//! Quitting with changes uploading waits for them, and quits on its own once
//! the last is on the server. In Rust because the drain is the sync status
//! changing under a running app, which only the fixture's handle can do; the
//! rest of the quit flow is `tests/js/headless/quit.test.js`.
use super::support::Fixture;
use gpui_agent::Mode;
use luma_lib::models::sync::SyncStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn uploading(pending: usize) -> SyncStatus {
    SyncStatus {
        connected: true,
        uploading: pending > 0,
        pending_uploads: pending,
        ..Default::default()
    }
}

#[test]
fn quitting_waits_for_the_queue_and_quits_when_it_drains() {
    let status = Arc::new(Mutex::new(uploading(4)));
    let mut harness = Fixture::new("quit-sync-drain", 4, vec![])
        .with_sync_status(status.clone())
        .open(Mode::Headless);
    let percent = r#"
        var uploaded = (s) => {
            const node = s.findAll((n) => n.role === "text" && n.label.startsWith("Uploaded "))[0];
            return node && Number(node.label.slice("Uploaded ".length, -1));
        };
    "#;
    harness.run(
        &format!(
            "{}\n{}\n{percent}\n{}",
            super::support::UNTIL,
            super::support::NAV,
            r#"
            nav.trackEditor("Test Venue", "Aurora");
            app.key("cmd-q");
            until("the dialog, nothing uploaded yet", (s) =>
                s.find({ role: "text", label: "4 changes left. Luma quits when they are uploaded." })
                && uploaded(s) === 0);
            true;
            "#
        ),
        Duration::from_secs(30),
    );

    // Half the queue up: the bar moves, and the app is still here.
    *status.lock().unwrap() = uploading(2);
    harness.run(
        &format!(
            "{}\n{percent}\n{}",
            super::support::UNTIL,
            r#"
            until("half uploaded", (s) => uploaded(s) > 0 && uploaded(s) < 100, { timeoutMs: 10000 });
            if (!app.isOpen()) throw new Error("quit with changes still waiting");
            true;
            "#
        ),
        Duration::from_secs(20),
    );

    // The last change up: the dialog quits by itself.
    *status.lock().unwrap() = uploading(0);
    harness.run(
        &format!(
            "{}\n{}",
            super::support::UNTIL,
            r#"
            until("the window to close", () => !app.isOpen(), { timeoutMs: 10000 });
            true;
            "#
        ),
        Duration::from_secs(20),
    );
}
