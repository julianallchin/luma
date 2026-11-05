//! The Perform verbs a Python cell calls, against a real library: what each
//! one stores, and what each refuses without storing anything.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use super::PerformHost;
use crate::agent_execution::worker_process::HostCallError;
use crate::database::local::{auth, database, state};
use crate::dispatch::{dispatch, AppServices};
use crate::models::perform::{CellContent, PerformLayout, PerformPattern};

const OWNER: &str = "11111111-2222-3333-4444-555555555555";

/// A signed-in library with one empty venue, and the host a Perform thread
/// of that venue gets.
async fn library() -> (tempfile::TempDir, AppServices, PerformHost) {
    let directory = tempfile::tempdir().unwrap();
    let db = database::init_app_db_at(directory.path()).await.unwrap();
    let state_db = state::init_state_db_at(directory.path()).await.unwrap();
    auth::install_test_session(&state_db.0, OWNER).await;
    auth::bootstrap_headless_admission(&db.0, &state_db.0)
        .await
        .unwrap();
    let storage = crate::storage::StorageRoot::from_path(directory.path().to_path_buf());
    let workspaces = Arc::new(
        crate::agent_execution::workspace::PythonWorkspaceService::new(
            storage.agent_workspaces_dir(),
            Arc::new(|| Err("no Python here".to_string())),
        ),
    );
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let services = AppServices::headless(db, state_db, storage, repo.clone(), workspaces);
    let venue = dispatch(
        &services,
        "create_venue",
        &json!({ "name": "Room", "description": null }),
    )
    .await
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let host = PerformHost::new(
        tokio::runtime::Handle::current(),
        services.db.0.clone(),
        repo,
        venue,
        Some("test-model".into()),
        services.perform.clone(),
        services.inputs.clone(),
    );
    (directory, services, host)
}

fn wash() -> Value {
    let preset = luma_patterns::presets()
        .clip("Wash")
        .expect("a Wash preset");
    json!({
        "name": "Warm wash",
        "selection": "all",
        "graph": preset.graph,
        "blend": preset.blend_mode,
    })
}

/// A graph with one fader node, `speed`.
fn with_fader() -> Value {
    json!({"version": 3, "nodes": {
        "speed": {"kind": "control", "settings": {"name": "speed"}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "speed"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}})
}

async fn call(host: &PerformHost, method: &str, payload: Value) -> Result<Value, HostCallError> {
    host.call(method, payload).await
}

fn patterns(answer: &Value) -> Vec<PerformPattern> {
    serde_json::from_value(answer["set"]["patterns"].clone()).unwrap()
}

fn layout(answer: &Value) -> PerformLayout {
    serde_json::from_value(answer["set"]["layout"].clone()).unwrap()
}

async fn stored(host: &PerformHost) -> Value {
    call(host, "perform.read", json!({})).await.unwrap()
}

#[tokio::test]
async fn a_pattern_from_a_preset_lands_in_its_cell() {
    let (_dir, _services, host) = library().await;
    let mut add = wash();
    add["at"] = json!([2, 1]);
    let added = call(&host, "perform.add", add).await.unwrap();
    let id = added["id"].as_str().unwrap();
    let [pattern] = &patterns(&added)[..] else {
        panic!("one pattern");
    };
    assert_eq!(pattern.id, id);
    assert_eq!(pattern.name, "Warm wash");
    let pad = pattern.graph.pad().expect("a new pattern gets a pad");
    assert_eq!(
        added["set"]["controls"]["patterns"][id]["pad"],
        json!(pad.node),
        "Python reads which node it is"
    );
    let cells: Vec<_> = layout(&added).cells_of(id).cloned().collect();
    assert_eq!(cells.len(), 1, "the pattern has one cell");
    assert_eq!((cells[0].column, cells[0].row), (2, 1));
    assert_eq!(added["cell"], json!(cells[0].id));

    // A second one with no place named takes the first free place, on top.
    let next = call(&host, "perform.add", wash()).await.unwrap();
    let set = patterns(&next);
    assert_eq!(
        set[0].id,
        next["id"].as_str().unwrap(),
        "the newest is on top"
    );
    assert!(set[0].z_index > set[1].z_index);
}

#[tokio::test]
async fn a_refused_change_stores_nothing() {
    let (_dir, _services, host) = library().await;
    let mut add = wash();
    add["at"] = json!([0, 0]);
    call(&host, "perform.add", add.clone()).await.unwrap();
    let before = stored(&host).await;

    let over = call(&host, "perform.add", add).await.unwrap_err();
    assert_eq!(over.code, "refused");
    assert!(over.message.contains("overlap"), "{}", over.message);

    let mut bad = wash();
    bad["graph"] = json!({"version": 3, "nodes": {
        "time1": {"kind": "time"},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "time1"}}}}});
    let unchecked = call(&host, "perform.add", bad).await.unwrap_err();
    assert_eq!(unchecked.code, "refused");
    assert!(
        unchecked.message.contains("color1.brightness: expected"),
        "the checker's own text: {}",
        unchecked.message
    );

    let mut nowhere = wash();
    nowhere["selection"] = json!("front_wsh");
    let unknown = call(&host, "perform.add", nowhere).await.unwrap_err();
    assert!(unknown.message.contains("front_wsh"), "{}", unknown.message);

    assert_eq!(stored(&host).await, before);
}

#[tokio::test]
async fn an_update_changes_what_it_names_and_a_removal_takes_its_cells() {
    let (_dir, _services, host) = library().await;
    let mut add = wash();
    add["graph"] = with_fader();
    let added = call(&host, "perform.add", add).await.unwrap();
    let id = added["id"].as_str().unwrap().to_owned();
    let seed = patterns(&added)[0].seed;

    let updated = call(
        &host,
        "perform.update",
        json!({"id": id, "name": "Cool wash", "graph": with_fader()}),
    )
    .await
    .unwrap();
    let pattern = &patterns(&updated)[0];
    assert_eq!(pattern.name, "Cool wash");
    assert_eq!(pattern.seed, seed, "what was not named stays");
    assert_eq!(
        pattern.graph.pad(),
        None,
        "an updated graph is stored as given"
    );
    assert_eq!(
        updated["set"]["controls"]["patterns"][&id]["pad"],
        Value::Null
    );
    let padless = call(&host, "perform.update", json!({"id": id, "pad": {}}))
        .await
        .unwrap_err();
    assert!(padless.message.contains("pad"), "{}", padless.message);

    // A fader cell on the pattern's level and its `speed` node.
    let fader = call(
        &host,
        "perform.place",
        json!({"content": {"kind": "fader", "label": "Speeds", "targets": [
            {"kind": "level", "patternId": id},
            {"kind": "node", "patternId": id, "node": "speed"}]}}),
    )
    .await
    .unwrap();
    let fader_id = fader["id"].as_str().unwrap().to_owned();

    let removed = call(&host, "perform.remove", json!({"id": id}))
        .await
        .unwrap();
    assert!(patterns(&removed).is_empty());
    let layout = layout(&removed);
    assert_eq!(layout.cells_of(&id).count(), 0);
    let cell = layout.pages[0]
        .cells
        .iter()
        .find(|cell| cell.id == fader_id)
        .unwrap();
    assert_eq!(
        cell.content,
        CellContent::Fader {
            label: "Speeds".into(),
            targets: Vec::new()
        },
        "the fader lets go of the pattern"
    );
    let gone = call(&host, "perform.remove", json!({"id": id}))
        .await
        .unwrap_err();
    assert_eq!(gone.code, "not_found");
}

#[tokio::test]
async fn reorder_puts_the_named_patterns_on_top_in_that_order() {
    let (_dir, _services, host) = library().await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        let added = call(&host, "perform.add", wash()).await.unwrap();
        ids.push(added["id"].as_str().unwrap().to_owned());
    }
    // Added bottom to top: ids[2] is on top.
    let reordered = call(&host, "perform.reorder", json!({"ids": [ids[0]]}))
        .await
        .unwrap();
    let order: Vec<String> = patterns(&reordered).into_iter().map(|p| p.id).collect();
    assert_eq!(order, [ids[0].clone(), ids[2].clone(), ids[1].clone()]);
    let twice = call(&host, "perform.reorder", json!({"ids": [ids[0], ids[0]]}))
        .await
        .unwrap_err();
    assert_eq!(twice.code, "refused");
}

#[tokio::test]
async fn pages_refuse_what_would_lose_a_cell_or_a_pattern() {
    let (_dir, _services, host) = library().await;
    let mut add = wash();
    add["at"] = json!([5, 3]);
    let added = call(&host, "perform.add", add).await.unwrap();
    let main = layout(&added).pages[0].id.clone();

    let small = call(&host, "perform.page", json!({"id": main, "columns": 4}))
        .await
        .unwrap_err();
    assert!(small.message.contains("does not fit"), "{}", small.message);
    let no_row = call(
        &host,
        "perform.page",
        json!({"id": main, "one_at_a_time": [9]}),
    )
    .await
    .unwrap_err();
    assert_eq!(no_row.code, "refused");
    let rows = call(
        &host,
        "perform.page",
        json!({"id": main, "one_at_a_time": [3, 0, 3]}),
    )
    .await
    .unwrap();
    assert_eq!(layout(&rows).pages[0].one_at_a_time, [0, 3]);

    let only = call(&host, "perform.remove_page", json!({"id": main}))
        .await
        .unwrap_err();
    assert!(
        only.message.contains("at least one page"),
        "{}",
        only.message
    );
    let second = call(&host, "perform.page", json!({"name": "Drops", "rows": 2}))
        .await
        .unwrap();
    let drops = second["id"].as_str().unwrap().to_owned();
    let held = call(&host, "perform.remove_page", json!({"id": main}))
        .await
        .unwrap_err();
    assert!(held.message.contains("holds patterns"), "{}", held.message);

    // The pattern's cell moves to the new page, to its first free place.
    let cell = added["cell"].as_str().unwrap();
    let moved = call(&host, "perform.cell", json!({"id": cell, "page": drops}))
        .await
        .unwrap();
    let pages = layout(&moved).pages;
    assert!(pages[0].cells.iter().all(|one| one.id != cell));
    let landed = pages[1].cells.iter().find(|one| one.id == cell).unwrap();
    assert_eq!((landed.column, landed.row), (0, 0));
    call(&host, "perform.remove_page", json!({"id": main}))
        .await
        .unwrap();
}

#[tokio::test]
async fn cells_refuse_targets_that_move_nothing() {
    let (_dir, _services, host) = library().await;
    let added = call(&host, "perform.add", wash()).await.unwrap();
    let id = added["id"].as_str().unwrap();
    let no_node = call(
        &host,
        "perform.place",
        json!({"content": {"kind": "fader", "label": "Speed", "targets": [
            {"kind": "node", "patternId": id, "node": "speed"}]}}),
    )
    .await
    .unwrap_err();
    assert!(
        no_node.message.contains("no fader node speed"),
        "{}",
        no_node.message
    );
    let as_pattern = call(
        &host,
        "perform.place",
        json!({"content": {"kind": "pattern", "patternId": id}}),
    )
    .await
    .unwrap_err();
    assert_eq!(as_pattern.code, "refused");
    let cell = added["cell"].as_str().unwrap();
    let label = call(&host, "perform.cell", json!({"id": cell, "label": "Hi"}))
        .await
        .unwrap_err();
    assert!(label.message.contains("not a fader"), "{}", label.message);
    let remove = call(&host, "perform.remove_cell", json!({"id": cell}))
        .await
        .unwrap_err();
    assert!(
        remove.message.contains("remove the pattern"),
        "{}",
        remove.message
    );

    let tap = call(
        &host,
        "perform.place",
        json!({"content": {"kind": "tap"}, "size": [1, 1]}),
    )
    .await
    .unwrap();
    let tap_id = tap["id"].as_str().unwrap();
    let gone = call(&host, "perform.remove_cell", json!({"id": tap_id}))
        .await
        .unwrap();
    assert!(layout(&gone).pages[0]
        .cells
        .iter()
        .all(|one| one.id != tap_id));
}

#[tokio::test]
async fn controls_play_only_this_venues_open_set() {
    let (_dir, services, host) = library().await;
    let added = call(&host, "perform.add", wash()).await.unwrap();
    let pad = crate::controls::perform::pad(&patterns(&added)[0]).expect("a pad");
    let closed = call(&host, "perform.press", json!({"name": pad, "down": true}))
        .await
        .unwrap_err();
    assert!(closed.message.contains("not open"), "{}", closed.message);
    assert_eq!(
        call(&host, "perform.live", json!({})).await.unwrap()["open"],
        false
    );

    services
        .perform
        .open_set(host.venue_id.clone())
        .await
        .unwrap();
    let on = call(&host, "perform.control", json!({"name": pad, "value": 1.0}))
        .await
        .unwrap();
    assert_eq!(on["open"], true);
    let button = &on["state"]["buttons"][&pad];
    assert!(button["gate"]["offFor"].is_null(), "{button}");
    let loud = call(&host, "perform.control", json!({"name": pad, "value": 2.0}))
        .await
        .unwrap_err();
    assert_eq!(loud.code, "refused");
}

#[tokio::test]
async fn bindings_name_what_they_change() {
    let (_dir, _services, host) = library().await;
    let listed = call(&host, "perform.bindings", json!({})).await.unwrap();
    assert_eq!(listed["inputs"]["bindings"], json!([]));
    let missing = call(
        &host,
        "perform.binding",
        json!({"id": "nope", "mode": "toggle", "invert": true}),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, "not_found");
    let named = call(
        &host,
        "perform.learn",
        json!({"control": "Not A Name", "mode": "fader"}),
    )
    .await
    .unwrap_err();
    assert_eq!(named.code, "refused");
}
