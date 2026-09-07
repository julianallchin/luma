//! Sync is quiet until a backlog or slow operation is worth showing.
#![cfg(feature = "app")]
use super::support::Fixture;
use gpui_agent::Mode;
use luma_lib::models::sync::{SyncProgress, SyncStatus};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn sync_status_shows_a_compact_backlog_and_expandable_live_counts() {
    let status = Arc::new(Mutex::new(SyncStatus {
        syncing: true,
        pending_changes: 12,
        progress: Some(SyncProgress {
            phase: "Uploading audio".into(),
            completed: 3,
            total: Some(8),
            unit: "files".into(),
        }),
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
        if (app.snapshot().find({role: "text", label: "Uploading audio"})) throw new Error("details should start collapsed");
        app.click(app.snapshot().find({role: "button", label: "Syncing · 12"}));
        until("live progress", s => s.find({role: "text", label: "3 / 8 files"}));
        if (app.snapshot().find({role: "button", label: "Retry sync"})) throw new Error("retry should not appear during sync");
        true;
    "#
    );
    let result = harness.exec(&script, Duration::from_secs(30));
    assert_eq!(result.error, None, "{result:?}");

    status.lock().unwrap().progress.as_mut().unwrap().completed = 6;
    let result = harness.exec(
        &format!(
            "{}\n{}",
            super::support::UNTIL,
            r#"
        until("updated count", s => s.find({role: "text", label: "6 / 8 files"}));
        true;
    "#
        ),
        Duration::from_secs(10),
    );
    assert_eq!(result.error, None, "{result:?}");

    status.lock().unwrap().progress.as_mut().unwrap().total = None;
    let result = harness.exec(&format!("{}\n{}", super::support::UNTIL, r#"
        until("unknown total", s => !s.find({role: "text", label: "6 / 8 files"}));
        if (app.snapshot().find({role: "text", label: "Finding work…"})) throw new Error("no placeholder paragraphs");
        if (app.snapshot().find({role: "text", label: "6 / 8 files"})) throw new Error("stale total");
        true;
    "#), Duration::from_secs(10));
    assert_eq!(result.error, None, "{result:?}");

    *status.lock().unwrap() = SyncStatus::default();
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
