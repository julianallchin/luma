//! Sync says one of five things; only a refusal opens, and a build too old
//! to sync is told so in place of the app.
use super::support::Fixture;
use gpui_agent::Mode;
use luma_lib::models::sync::{SyncRefusal, SyncStatus};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

fn step(harness: &mut gpui_agent::Harness, body: &str) {
    harness.run(
        &format!("{}\n{}\ntrue;", super::support::UNTIL, body),
        Duration::from_secs(20),
    );
}

/// `Syncing`, `Offline · N pending`, `N not saved` and `Up to date`, each as
/// the status bar's one sync item. A transport error never shows: the
/// connection it broke reads as offline.
#[test]
fn sync_status_reads_in_four_states_and_only_a_refusal_opens() {
    let status = Arc::new(Mutex::new(SyncStatus {
        connected: true,
        uploading: true,
        pending_uploads: 12,
        ..Default::default()
    }));
    let mut harness = Fixture::new("sync-states", 4, vec![])
        .with_sync_status(status.clone())
        .open(Mode::Headless);
    let script = format!(
        "{}\n{}",
        super::support::NAV,
        r#"
        nav.trackEditor("Test Venue", "Aurora");
        until("a backlog syncing", s => s.find({role: "text", label: "Syncing"}));
        true;
    "#
    );
    harness.run(&script, Duration::from_secs(30));

    // Offline is a fact, not a fault: the changes wait and say how many.
    *status.lock().unwrap() = SyncStatus {
        connected: false,
        pending_uploads: 3,
        ..Default::default()
    };
    step(
        &mut harness,
        r#"until("offline", s => s.find({role: "text", label: "Offline · 3 pending"}), { timeoutMs: 10000 });"#,
    );

    // Refused changes outrank the rest, and are the one item that opens.
    *status.lock().unwrap() = SyncStatus {
        connected: true,
        refused: vec![
            refusal("r1", "tracks", "PATCH"),
            refusal("r2", "fixtures", "DELETE"),
        ],
        ..Default::default()
    };
    step(
        &mut harness,
        r#"
        const shown = until("not saved", s => s.find({role: "button", label: "2 not saved"}));
        app.click(shown.find({role: "button", label: "2 not saved"}));
        until("the refused list", s => s.find({role: "text", label: "Edit to tracks"})
            && s.find({role: "text", label: "Removal from fixtures"}));
        app.click(app.snapshot().find({role: "button", label: "Dismiss"}));
        until("dismissed", s => s.find({role: "text", label: "Up to date"})
            && !s.find({role: "button", label: "2 not saved"})
            && !s.find({role: "text", label: "Edit to tracks"}));
    "#,
    );
    assert!(
        status.lock().unwrap().refused.is_empty(),
        "Dismiss reached the library"
    );
}

/// A build behind the server's data is told so in place of the app, with
/// what it costs to carry on; put off, the app is back and the status bar
/// keeps saying it.
#[test]
fn a_required_update_stands_in_for_the_app_until_it_is_put_off() {
    let status = Arc::new(Mutex::new(SyncStatus {
        connected: true,
        ..Default::default()
    }));
    let mut harness = Fixture::new("sync-update-required", 4, vec![])
        .with_sync_status(status.clone())
        .open(Mode::Headless);
    let script = format!(
        "{}\n{}",
        super::support::NAV,
        r#"
        nav.trackEditor("Test Venue", "Aurora");
        until("in sync", s => s.find({role: "text", label: "Up to date"}));
        true;
    "#
    );
    harness.run(&script, Duration::from_secs(30));

    status.lock().unwrap().update_required = true;
    step(
        &mut harness,
        r#"
        const asked = until("the update is asked for", s =>
            s.find({role: "text", label: "Update Luma to continue"})
            && s.find({role: "text", label: "Changes made before updating are not saved."})
            && !s.find({role: "text", label: "Up to date"}));
        app.click(asked.find({role: "button", label: "Not Now"}));
        until("the app is back, still saying so", s =>
            s.find({role: "button", label: "Update required"})
            && !s.find({role: "text", label: "Update Luma to continue"}));
    "#,
    );
}
