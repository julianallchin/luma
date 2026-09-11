//! Sync is quiet until a backlog, a slow operation or a fault is worth showing.
#![cfg(feature = "app")]
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
    let result = harness.exec(&script, Duration::from_secs(30));
    assert_eq!(result.error, None, "{result:?}");

    *status.lock().unwrap() = SyncStatus {
        connected: false,
        error: Some("upload refused".into()),
        ..Default::default()
    };
    let result = harness.exec(
        &format!(
            "{}\n{}",
            super::support::UNTIL,
            r#"
        until("fault", s => s.find({role: "button", label: "Sync needs attention"}));
        app.click(app.snapshot().find({role: "button", label: "Sync needs attention"}));
        until("reason", s => s.find({role: "text", label: "upload refused"}));
        until("retry offered", s => s.find({role: "button", label: "Retry sync"}));
        true;
    "#
        ),
        Duration::from_secs(10),
    );
    assert_eq!(result.error, None, "{result:?}");

    *status.lock().unwrap() = SyncStatus {
        connected: true,
        ..Default::default()
    };
    let result = harness.exec(
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
    assert_eq!(result.error, None, "{result:?}");
}
