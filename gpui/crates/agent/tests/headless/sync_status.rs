//! Sync is quiet until a backlog, a slow operation or a fault is worth showing.
use super::support::Fixture;
use gpui_agent::Mode;
use luma_lib::models::sync::SyncStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn sync_status_shows_a_compact_backlog_and_expandable_detail() {
    let status = Arc::new(Mutex::new(SyncStatus {
        connected: true,
        uploading: true,
        pending_uploads: 12,
        ..Default::default()
    }));
    let mut harness = Fixture::new("sync-progress", 4, vec![])
        .with_sync_status(status.clone())
        .open(Mode::Headless);
    let script = format!(
        "{}\n{}",
        super::support::NAV,
        r#"
        nav.trackEditor("Test Venue", "Aurora");
        until("sync backlog", s => s.find({role: "button", label: "Syncing · 12"}));
        true;
    "#
    );
    harness.run(&script, Duration::from_secs(30));

    // A transport fault reads on the row itself. PowerSync retries on its own,
    // so there is nothing to press — what the row owes the user is the reason,
    // and "Sync needs attention" is not one.
    *status.lock().unwrap() = SyncStatus {
        connected: false,
        error: Some("Sync rejected this sign-in. 401 from the endpoint".into()),
        ..Default::default()
    };
    harness.run(
        &format!(
            "{}\n{}",
            super::support::UNTIL,
            r#"
        until("fault", s => s.find({role: "button", label: "Sync rejected this sign-in"}));
        app.click(app.snapshot().find({role: "button", label: "Sync rejected this sign-in"}));
        until("reason", s => s.find({role: "text", label: "Sync rejected this sign-in. 401 from the endpoint"}));
        true;
    "#
        ),
        Duration::from_secs(10),
    );

    *status.lock().unwrap() = SyncStatus {
        connected: true,
        ..Default::default()
    };
    harness.run(
        &format!(
            "{}\n{}",
            super::support::UNTIL,
            r#"
        until("finished", s => s.find({role: "button", label: "Up to date"}));
        app.click(app.snapshot().find({role: "button", label: "Up to date"}));
        until("quiet idle", s => !s.find({role: "button", label: "Up to date"}));
        true;
    "#
        ),
        Duration::from_secs(10),
    );
}
