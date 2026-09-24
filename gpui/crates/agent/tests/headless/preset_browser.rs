//! The preset browser from the outside: with no clip selected the inspector
//! lists the shipped presets by form, one row each with a strip; the search
//! filters them; a click and a drag place a form clip; hovering a row plays
//! the preset on the stage and leaving it gives the stage back, with the
//! score untouched.

use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

fn stored(name: &str) -> serde_json::Value {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(support::stored_score_json(&support::config_dir(name)))
}

fn labels(value: &serde_json::Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect()
}

#[test]
fn the_browser_lists_presets_by_form_filters_and_places_on_click() {
    let name = "preset-browser-click";
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(support::score(serde_json::json!({})))
        .with_rig()
        .window(1400., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const inBrowser=role=>{
            const p=node("card","Presets").bounds;
            const inside=n=>n.bounds.x>=p.x&&n.bounds.x<p.x+p.width&&n.bounds.y>=p.y&&n.bounds.y<p.y+p.height;
            return app.snapshot().findAll({role}).filter(inside).map(n=>n.label);
        };
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        node("card","Presets");
        const inspector=!!app.snapshot().find({role:"card",label:"Clip inputs"});
        const width=node("card","Presets").bounds.width;
        const captions=inBrowser("text");
        const tiles=inBrowser("row");
        // Each tile shows the preset's strip on this rig once it renders.
        until("thumbnails",s=>s.find({role:"card",label:"Wash thumbnail"})&&s.find({role:"card",label:"Chase thumbnail"}));
        app.type(node("input","Search presets…"),"chase"); app.frames(2);
        const filtered=inBrowser("row");
        app.click(node("row","Bounce"));
        until("clip inputs",s=>s.find({role:"card",label:"Clip inputs"}));
        const browserGone=!app.snapshot().find({role:"card",label:"Presets"});
        app.click(node("card","Waveform"));
        until("browser back",s=>s.find({role:"card",label:"Presets"}));
        ({inspector,width,captions,tiles,filtered,browserGone})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    assert_eq!(out["inspector"], false, "{out}");
    assert!(
        (out["width"].as_f64().unwrap() - 320.).abs() < 1.,
        "the inspector is open with nothing selected: {out}"
    );
    // The list scrolls; the forms on screen come first, in shipped order.
    let captions = labels(&out["captions"]);
    let forms = [
        "Constant color",
        "Color over time",
        "Color across space",
        "Chase",
        "Sparkle",
        "Noise",
        "Strobe",
    ];
    assert!(captions.len() >= 4, "{out}");
    assert_eq!(captions, forms[..captions.len()], "{out}");
    let tiles = labels(&out["tiles"]);
    assert_eq!(
        tiles[..4],
        ["Wash", "Color fade", "Rainbow", "Gradient"],
        "{out}"
    );
    let filtered = labels(&out["filtered"]);
    assert!(
        filtered.contains(&"Chase") && filtered.contains(&"Wave") && filtered.contains(&"Bounce"),
        "a search matches the form's name: {out}"
    );
    assert!(!filtered.contains(&"Wash"), "{out}");
    assert_eq!(out["browserGone"], true, "a placed clip is selected: {out}");

    let score = stored(name);
    let clips = score["clips"].as_object().unwrap();
    assert_eq!(clips.len(), 1, "{score}");
    let clip = clips.values().next().unwrap();
    assert_eq!(clip["graph"], "color.chase@1");
    assert_eq!(clip["selection"]["expression"], "all");
    let bounce = luma_patterns::presets().preset("Bounce").unwrap();
    assert_eq!(
        clip["inputs"],
        serde_json::to_value(&bounce.inputs).unwrap(),
        "a placed clip copies every value of its preset"
    );
    assert!(
        (clip["duration"].as_f64().unwrap() - 16.).abs() < 0.01,
        "a click places four bars: {clip}"
    );
}

#[test]
fn a_row_dragged_onto_the_timeline_shows_where_it_lands_and_lands_there() {
    let name = "preset-browser-drag";
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(support::score(serde_json::json!({})))
        .with_rig()
        .window(1400., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        const lane=node("row","Lane 0").bounds;
        const tile=node("row","Ripple");
        const ghosts=()=>app.painted().flatMap(s=>s.findAll({role:"card",label:"Ripple drop preview"}));
        // Carried off the timeline and let go: nothing lands.
        const search=node("input","Search presets…");
        app.drag(tile,{dx:0,dy:search.bounds.y+search.bounds.height/2-(tile.bounds.y+tile.bounds.height/2)},{steps:6,restale:"match"});
        app.frames(2);
        const cancelled={ghost:!!app.snapshot().find({role:"card",label:"Ripple drop preview"}),
                         browser:!!app.snapshot().find({role:"card",label:"Presets"})};
        const from=node("row","Ripple");
        app.drag(from,{dx:lane.x+lane.width/2-(from.bounds.x+from.bounds.width/2),dy:lane.y+lane.height/2-(from.bounds.y+from.bounds.height/2)},{steps:12,restale:"match"});
        // While over the lane, the timeline drew the clip it would make.
        const seen=ghosts().map(n=>n.bounds);
        until("clip inputs",s=>s.find({role:"card",label:"Clip inputs"}));
        const after=!!app.snapshot().find({role:"card",label:"Ripple drop preview"});
        // The placed clip is written after a round trip.
        app.frames(8,{waitMs:80});
        ({cancelled,seen,after,lane})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    assert_eq!(
        out["cancelled"],
        serde_json::json!({"ghost": false, "browser": true}),
        "a drag let go off the timeline places nothing: {out}"
    );
    let seen = out["seen"].as_array().unwrap();
    assert!(
        !seen.is_empty(),
        "no drop preview while over the lane: {out}"
    );
    let last = seen.last().unwrap();
    let lane_y = out["lane"]["y"].as_f64().unwrap();
    assert!(
        (last["y"].as_f64().unwrap() - lane_y).abs() < 4.,
        "the drop preview sits in the lane under the pointer: {out}"
    );
    assert_eq!(out["after"], false, "the drop preview goes on drop: {out}");
    let score = stored(name);
    let clips = score["clips"].as_object().unwrap();
    assert_eq!(clips.len(), 1, "{score}");
    let clip = clips.values().next().unwrap();
    let ripple = luma_patterns::presets().preset("Ripple").unwrap();
    assert_eq!(clip["graph"], "color.chase@1");
    assert_eq!(
        clip["inputs"],
        serde_json::to_value(&ripple.inputs).unwrap()
    );
    assert!(
        clip["start"].as_f64().unwrap() > 0.,
        "the drop places the clip where it is let go, not at the playhead: {clip}"
    );
}

#[test]
fn hovering_a_row_plays_it_on_the_stage_until_the_pointer_leaves() {
    let name = "preset-browser-hover";
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(support::score(serde_json::json!({})))
        .with_rig()
        .window(1400., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const badge=s=>s.find({role:"text",label:"Preview · Gradient"});
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        node("card","Stage");
        const before=!!badge(app.snapshot());
        // A wheel of nothing moves the pointer onto a control and leaves it there.
        app.scroll(node("row","Gradient"),{dy:0});
        until("the stage plays the preset",s=>badge(s));
        app.scroll(node("input","Search presets…"),{dy:0});
        app.frames(1);
        const after=!!badge(app.snapshot());
        const placed=app.snapshot().findAll({role:"card",label:"Gradient"}).length;
        ({before,after,placed})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    assert_eq!(out["before"], false, "{out}");
    assert_eq!(
        out["after"], false,
        "leaving the tile gives the stage back: {out}"
    );
    assert_eq!(
        out["placed"], 0,
        "hovering does not change the score: {out}"
    );
}
