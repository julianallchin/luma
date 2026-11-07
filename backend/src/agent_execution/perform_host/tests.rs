//! The Perform verbs a Python cell calls, against a real library: what each
//! one stores, and what each refuses without storing anything.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::PerformHost;
use crate::agent_execution::worker_process::HostCallError;
use crate::dispatch::{dispatch, AppServices};
use crate::models::perform::{CellContent, PerformLayout, PerformPattern};

/// A signed-in library with one empty venue, and the host a Perform thread
/// of that venue gets.
async fn library() -> (tempfile::TempDir, AppServices, PerformHost) {
    let directory = tempfile::tempdir().unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let services = crate::test_support::signed_in(directory.path(), repo.clone()).await;
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

/// `wash()` with its pad, `start` (a new pad starts on the beat).
fn wash_starting(start: &str) -> Value {
    let mut add = wash();
    let mut graph: luma_patterns::ClipGraph = serde_json::from_value(add["graph"].take()).unwrap();
    graph = graph.with_pad();
    let pad = graph.pad().expect("a pad").node;
    graph
        .nodes
        .get_mut(&pad)
        .unwrap()
        .settings
        .insert("start".into(), start.into());
    add["graph"] = json!(graph);
    add
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

/// A graph with a fader node, `speed`, and a live color, `hue`, on its
/// color.
fn with_color() -> Value {
    json!({"version": 3, "nodes": {
        "speed": {"kind": "control", "settings": {"name": "speed"}},
        "hue": {"kind": "control", "settings": {"name": "main", "kind": "color"}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "speed"}}},
        "color1": {"kind": "color", "inputs": {
            "color": {"node": "hue"}, "brightness": {"node": "curve1"}}}}})
}

#[tokio::test]
async fn a_color_cell_sets_live_colors_and_changes_as_told() {
    let (_dir, _services, host) = library().await;
    let mut add = wash();
    add["graph"] = with_color();
    let added = call(&host, "perform.add", add).await.unwrap();
    let id = added["id"].as_str().unwrap();
    let node = |node: &str| json!({"kind": "node", "patternId": id, "node": node});
    let colors = |target: Value| {
        json!({"content": {"kind": "color", "label": "Colors",
            "swatches": [[1, 0, 0], [0, 0, 1]], "fade": 1, "unit": "beats",
            "targets": [target], "step": {"bars": 4, "loops": true}}})
    };
    let fader = call(&host, "perform.place", colors(node("speed")))
        .await
        .unwrap_err();
    assert_eq!(fader.code, "refused");
    assert!(
        fader.message.contains("no live color node speed"),
        "{}",
        fader.message
    );
    let level = call(
        &host,
        "perform.place",
        colors(json!({"kind": "level", "patternId": id})),
    )
    .await
    .unwrap_err();
    assert_eq!(level.code, "refused", "a level is a number");
    let hue = call(
        &host,
        "perform.place",
        json!({"content": {"kind": "fader", "label": "Speed", "targets": [node("hue")]}}),
    )
    .await
    .unwrap_err();
    assert!(hue.message.contains("no fader node hue"), "{}", hue.message);

    let placed = call(&host, "perform.place", colors(node("hue")))
        .await
        .unwrap();
    let cell = placed["id"].as_str().unwrap();
    assert_eq!(
        placed["set"]["controls"]["cells"][cell],
        json!([
            crate::controls::perform::swatch(cell, 0),
            crate::controls::perform::swatch(cell, 1)
        ]),
        "a control per swatch"
    );
    let content = |answer: &Value| {
        layout(answer).pages[0]
            .cells
            .iter()
            .find(|one| one.id == cell)
            .unwrap()
            .content
            .clone()
    };
    let changed = call(
        &host,
        "perform.cell",
        json!({"id": cell, "swatches": [[0, 1, 0]], "fade": 2, "unit": "seconds"}),
    )
    .await
    .unwrap();
    let CellContent::Color {
        swatches,
        fade,
        unit,
        step,
        ..
    } = content(&changed)
    else {
        panic!("still a color cell");
    };
    assert_eq!((swatches, fade), (vec![[0., 1., 0.]], 2.));
    assert_eq!(unit, luma_patterns::press::TimeUnit::Seconds);
    assert!(step.is_some(), "a step not given stays");
    let still = call(&host, "perform.cell", json!({"id": cell, "step": null}))
        .await
        .unwrap();
    let CellContent::Color { step, .. } = content(&still) else {
        panic!("still a color cell");
    };
    assert_eq!(step, None, "a step of null stops it stepping");
    let none = call(&host, "perform.cell", json!({"id": cell, "swatches": []}))
        .await
        .unwrap_err();
    assert!(none.message.contains("swatches"), "{}", none.message);
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

/// Give the host's venue an empty group `name`.
async fn group(services: &AppServices, host: &PerformHost, name: &str) {
    use crate::database::local::venue_access::{VenueAccess, VenueResource, Write};
    let mut access =
        VenueAccess::<Write>::write(&services.db.0, VenueResource::Venue(&host.venue_id))
            .await
            .unwrap();
    crate::database::local::groups::create_group(&mut access, Some(name), None, None, None)
        .await
        .unwrap();
    access.commit().await.unwrap();
}

#[tokio::test]
async fn a_pattern_plays_on_targets_as_told() {
    let (_dir, services, host) = library().await;
    group(&services, &host, "front").await;
    let added = call(&host, "perform.add", wash_starting("now"))
        .await
        .unwrap();
    let pad = crate::controls::perform::pad(&patterns(&added)[0]).expect("a pad");
    let target = |group: &str| json!({"content": {"kind": "target", "label": "", "group": group}});
    let unknown = call(&host, "perform.place", target("nowhere"))
        .await
        .unwrap_err();
    assert!(unknown.message.contains("nowhere"), "{}", unknown.message);
    let placed = call(&host, "perform.place", target("front")).await.unwrap();
    let cell = placed["id"].as_str().unwrap().to_owned();
    assert_eq!(
        placed["set"]["controls"]["cells"][&cell],
        crate::controls::perform::cell(&cell),
        "a target is held by its control"
    );

    services
        .perform
        .open_set(host.venue_id.clone())
        .await
        .unwrap();
    let play = |value: f64, on: &[&str]| {
        call(
            &host,
            "perform.control",
            json!({"name": pad, "value": value, "on": on}),
        )
    };
    let on = play(1.0, &["front"]).await.unwrap();
    let instance = crate::controls::perform::instance(&pad, &["front".to_owned()]);
    assert_eq!(
        on["state"]["instances"][&instance],
        json!({"patternId": patterns(&added)[0].id, "on": ["front"]})
    );
    assert!(on["state"]["buttons"][&instance]["gate"]["offFor"].is_null());
    assert!(
        on["state"]["buttons"][&pad]["gate"].is_null(),
        "its own selection stays off"
    );
    let off = play(0.0, &["front"]).await.unwrap();
    assert!(!off["state"]["buttons"][&instance]["gate"]["offFor"].is_null());
    let refused = play(1.0, &["nowhere"]).await.unwrap_err();
    assert_eq!(refused.code, "refused", "{}", refused.message);
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

#[tokio::test]
async fn a_press_on_the_bar_waits_for_it_and_live_says_so() {
    let (_dir, services, host) = library().await;
    let added = call(&host, "perform.add", wash()).await.unwrap();
    let pattern = &patterns(&added)[0];
    let luma_patterns::clip_graph::LiveInputKind::Button(press) =
        pattern.graph.pad().expect("a pad").kind
    else {
        panic!("a pad is a button");
    };
    assert_eq!(
        press.start,
        luma_patterns::press::Start::Beat,
        "a new pad starts on the beat"
    );

    let id = pattern.id.clone();
    call(
        &host,
        "perform.update",
        json!({"id": id, "graph": wash_starting("bar")["graph"]}),
    )
    .await
    .unwrap();
    services
        .perform
        .open_set(host.venue_id.clone())
        .await
        .unwrap();
    // At 30 BPM a bar is eight seconds: the press waits.
    services.controls.venue(&host.venue_id).set_bpm(30.0);
    let pad = crate::controls::perform::pad(&patterns(&stored(&host).await)[0]).expect("a pad");
    call(&host, "perform.press", json!({"name": pad, "down": true}))
        .await
        .unwrap();
    let live = call(&host, "perform.press", json!({"name": pad, "down": false}))
        .await
        .unwrap();
    let button = &live["state"]["buttons"][&pad];
    assert_eq!(button["press"]["start"], "bar", "{button}");
    assert!(
        button["gate"]["onFor"].as_f64().unwrap() < 0.0,
        "on at a bar still to come: {button}"
    );
}

#[tokio::test]
async fn a_busk_page_is_added_beside_the_set_and_plays() {
    let (_dir, services, host) = library().await;
    let kept = call(&host, "perform.add", wash()).await.unwrap();
    let kept = patterns(&kept);
    let made = call(&host, "perform.busk", json!({})).await.unwrap();
    let page = made["id"].as_str().unwrap();
    let pages = layout(&made).pages;
    assert_eq!(pages.len(), 2, "a page of its own");
    let busk = pages.iter().find(|one| one.id == page).unwrap();
    assert_eq!(busk.name, "Busk");
    let after = patterns(&made);
    assert!(
        after.contains(&kept[0]),
        "the set's pattern stays as it was"
    );
    assert!(after
        .iter()
        .filter(|pattern| pattern.id != kept[0].id)
        .all(|pattern| pattern.z_index > kept[0].z_index));
    // A room with no groups has one target, everything.
    let targets: Vec<&CellContent> = busk
        .cells
        .iter()
        .map(|cell| &cell.content)
        .filter(|content| matches!(content, CellContent::Target { .. }))
        .collect();
    assert!(matches!(targets[..], [CellContent::Target { group, .. }] if group == "all"));

    let again = call(&host, "perform.busk", json!({})).await.unwrap();
    let names: Vec<String> = layout(&again)
        .pages
        .into_iter()
        .map(|one| one.name)
        .collect();
    assert_eq!(names[1..], ["Busk", "Busk 2"]);
    services
        .perform
        .open_set(host.venue_id.clone())
        .await
        .unwrap();
}
