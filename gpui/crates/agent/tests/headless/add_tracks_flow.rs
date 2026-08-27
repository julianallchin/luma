//! Outside-in add-track flow: shared browser, normalized source navigation,
//! durable import, close-not-cancel, newest-row reconciliation, and explicit
//! venue membership.

#![cfg(all(feature = "app", unix))]

use super::support;

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use gpui_agent::{Harness, Mode};
use serde_json::{json, Value};
use support::Fixture;

fn silent_wav(path: &Path) {
    let frames = 8_000_u32;
    let data_len = frames * 2;
    let mut bytes = Vec::with_capacity((44 + data_len) as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8_000_u32.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize((44 + data_len) as usize, 0);
    std::fs::write(path, bytes).unwrap();
}

fn install_slow_analysis(cache: &Path) {
    let bin = cache.join("python-env/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let python = bin.join("python3");
    std::fs::write(
        &python,
        "#!/bin/sh\nsleep 0.4\ncase \"$1\" in\n  *beat_worker.py) printf '%s\\n' '{\"beats\":[0.0,0.5],\"downbeats\":[0.0],\"bpm\":120.0,\"downbeat_offset\":0.0,\"beats_per_bar\":4}' ;;\n  *) exit 1 ;;\nesac\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&python).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(python, permissions).unwrap();
}

fn harness() -> Harness {
    let external = std::env::temp_dir().join(format!(
        "luma-gpui-add-tracks-source-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&external).ok();
    std::fs::create_dir_all(&external).unwrap();
    let audio = external.join("needle.wav");
    silent_wav(&audio);
    let cache = external.join("cache");
    install_slow_analysis(&cache);
    std::env::set_var("LUMA_CACHE_DIR", cache);

    let track = json!({
        "id": "content-id",
        "uuid": "rb-uuid",
        "filePath": audio,
        "filename": "needle.wav",
        "title": "Needle",
        "artist": "Artist",
        "album": "Album",
        "bpm": 128.0,
        "durationSeconds": 1.0,
        "fileSize": 16044,
        "sampleRate": 8000
    });
    Fixture::new("add-tracks-flow", 20, vec![])
        .with_track_created_at("2000-01-01 00:00:00")
        .with_equal_timestamp_track()
        .with_source_fixture(luma_app::SourceAdapterFixture {
            library: json!({"trackCount": 1}),
            playlists: json!([{
                "id": "crate-1",
                "name": "Warmup crate",
                "parentId": null,
                "trackCount": 1
            }]),
            tracks: json!([track.clone()]),
            playlist_tracks: HashMap::from([("crate-1".into(), json!([track.clone()]))]),
            searches: HashMap::from([("needle".into(), json!([track]))]),
        })
        .with_source_import_fixture_delay(Duration::from_millis(350))
        .open(Mode::Headless)
}

const SCRIPT: &str = r#"
    // A track is on screen twice while this dialog is open: the venue's own
    // list *behind* it (x 0, width 255) and the browser's row *inside* it
    // (x 268, width 664). Only the second is what any step here means, and
    // width is the only thing that tells them apart.
    //
    // So `find({role: "row", label})` alone is not a wait for the browser — it
    // is satisfied instantly by the row behind the dialog, before the browser
    // has laid out. Every predicate below waits on this instead.
    const BROWSER_ROW_MIN_WIDTH = 300;
    const browserRow = (s, label) => s.findAll({ role: "row", label })
        .find((row) => row.bounds.width > BROWSER_ROW_MIN_WIDTH);

    nav.venue("Test Venue");
    nav.step("the add-track affordance", "button", "Add track");
    const initial = until("the all-Luma browser", (s) =>
        s.find({ role: "input", label: "Search all tracks…" }) !== undefined
            && browserRow(s, "Aurora") !== undefined
            && s.find({ role: "button", label: "Import tracks" }) !== undefined);
    const auroraRow = browserRow(initial, "Aurora");
    const initialRows = initial.findAll({ role: "row" })
        .filter((row) => Math.abs(row.bounds.x - auroraRow.bounds.x) < 1
            && Math.abs(row.bounds.width - auroraRow.bounds.width) < 1)
        .map((row) => row.label);

    // The browser searches the global Luma library, not only the active
    // venue's membership, and updates the assembled production row list.
    app.click(initial.find({ role: "input", label: "Search all tracks…" }));
    app.key("z");
    const globalSearch = until("the global Luma track search", (s) =>
        browserRow(s, "Zulu") !== undefined && browserRow(s, "Aurora") === undefined);
    app.key("backspace");
    const restoredGlobal = until("the restored global library", (s) =>
        browserRow(s, "Zulu") !== undefined && browserRow(s, "Aurora") !== undefined);

    // Existing rows use the same explicit membership operation as imported
    // rows. It is idempotent for Aurora's already-present empty score.
    app.click(browserRow(restoredGlobal, "Aurora"));
    const auroraMember = until("the idempotent existing membership", (s) =>
        s.find({ role: "text", label: "Aurora venue scores: 1" }) !== undefined
            && s.find({ role: "input", label: "Search all tracks…" }) === undefined);

    nav.step("the add-track affordance again", "button", "Add track");
    const browser = until("the reopened all-Luma browser", (s) =>
        s.find({ role: "button", label: "Import tracks" }) !== undefined);
    app.click(browser.find({ role: "button", label: "Import tracks" }));
    const picker = until("the import-source menu", (s) =>
        s.find({ role: "row", label: "Engine DJ" }) !== undefined
            && s.find({ role: "row", label: "Rekordbox" }) !== undefined
            && s.find({ role: "row", label: "Files…" }) !== undefined);
    app.click(picker.find({ role: "row", label: "Rekordbox" }));
    const source = until("the normalized Rekordbox library", (s) =>
        s.find({ role: "row", label: "Warmup crate" }) !== undefined
            && s.find({ role: "row", label: "Needle" }) !== undefined
            && s.find({ role: "input", label: "Search source…" })?.focused === true);

    // Search and playlists both replace the same normalized row model.
    app.click(source.find({ role: "input", label: "Search source…" }));
    for (const key of ["n", "e", "e", "d", "l", "e"]) app.key(key);
    const searched = until("the source search result", (s) =>
        s.find({ role: "row", label: "Needle" }) !== undefined);
    app.click(searched.find({ role: "row", label: "Warmup crate" }));
    const crate = until("the crate rows", (s) =>
        s.find({ role: "row", label: "Needle" }) !== undefined);
    app.click(crate.find({ role: "row", label: "Needle" }));
    const selected = until("the enabled import", (s) =>
        s.find({ role: "button", label: "Import selected" })?.enabled === true);
    app.click(selected.find({ role: "button", label: "Import selected" }));

    // Dismiss immediately, then reopen while the fixture still holds phase one
    // before insertion. App-owned import state must outlive both route trees.
    app.click(app.snapshot().find({ role: "button", label: "Close" }));
    const closed = until("the closed import dialog", (s) =>
        s.find({ role: "button", label: "Close" }) === undefined);
    nav.step("the add-track affordance before insertion", "button", "Add track");
    const reopenedBeforeInsert = until("the reopened pre-insert browser", (s) =>
        s.find({ role: "button", label: "Import tracks" }) !== undefined
            && s.find({ role: "chip", label: "Track import phase: importing" }) !== undefined
            && s.find({ role: "row", label: "Needle" }) === undefined);
    const analyzing = until("structured analysis progress after reopen", (s) =>
        s.find({ role: "chip", label: "Track import phase: analyzing" }) !== undefined
            && s.find({ role: "text", label: "Track import progress: 0/1" }) !== undefined
            && s.find({ role: "row", label: "Needle" }) !== undefined);
    const reconciled = until("terminal import event and enriched row", (s) =>
        s.find({ role: "chip", label: "Track import phase: complete" }) !== undefined
            && s.find({ role: "text", label: "Track import progress: 1/1" }) !== undefined
            && s.find({ role: "row", label: "Needle" }) !== undefined);
    const importedRow = reconciled.find({ role: "row", label: "Needle" });
    const rows = reconciled.findAll({ role: "row" })
        .filter((row) => Math.abs(row.bounds.x - importedRow.bounds.x) < 1
            && Math.abs(row.bounds.width - importedRow.bounds.width) < 1)
        .map((row) => row.label);
    app.click(reconciled.find({ role: "row", label: "Needle" }));
    const member = until("Needle in the active venue exactly once", (s) =>
        s.find({ role: "text", label: "Needle venue scores: 1" }) !== undefined
            && s.find({ role: "button", label: "Close" }) === undefined);

    nav.step("the add-track affordance for an idempotent retry", "button", "Add track");
    const retry = until("Needle in the retry browser", (s) =>
        browserRow(s, "Needle") !== undefined);
    app.click(browserRow(retry, "Needle"));
    const retried = until("one score after repeated membership", (s) =>
        s.find({ role: "text", label: "Needle venue scores: 1" }) !== undefined
            && s.find({ role: "button", label: "Close" }) === undefined);

    ({ closed: closed.find({ role: "button", label: "Close" }) === undefined,
       initialRows,
       globalSearch: globalSearch.find({ role: "row", label: "Zulu" }) !== undefined,
       auroraIdempotent: auroraMember.find({ role: "text", label: "Aurora venue scores: 1" }) !== undefined,
       reopenedBeforeInsert: reopenedBeforeInsert.find({ role: "row", label: "Needle" }) === undefined,
       analyzing: analyzing.find({ role: "chip", label: "Track import phase: analyzing" }) !== undefined,
       terminal: reconciled.find({ role: "chip", label: "Track import phase: complete" }) !== undefined,
       rows,
       member: member.find({ role: "text", label: "Needle venue scores: 1" }) !== undefined,
       retried: retried.find({ role: "text", label: "Needle venue scores: 1" }) !== undefined })
"#;

#[test]
fn source_import_survives_close_reconciles_newest_and_joins_the_venue() {
    let mut harness = harness();
    let result = harness.exec(&support::script(SCRIPT), Duration::from_secs(300));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    let out: Value = result.result;
    assert_eq!(out["closed"], true);
    assert_eq!(out["initialRows"], json!(["Zulu", "Aurora"]));
    assert_eq!(out["globalSearch"], true);
    assert_eq!(out["auroraIdempotent"], true);
    assert_eq!(out["reopenedBeforeInsert"], true);
    assert_eq!(out["analyzing"], true);
    assert_eq!(out["terminal"], true);
    assert_eq!(out["member"], true);
    assert_eq!(out["retried"], true);
    assert_eq!(
        out["rows"][0], "Needle",
        "import did not reconcile newest-first"
    );
}
