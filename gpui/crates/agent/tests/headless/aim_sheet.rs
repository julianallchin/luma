//! An aim clip from the outside: placed from the preset browser, its sheet
//! shows the base's rows and the motion's rows that apply, direction as turn
//! and tilt, and no blend mode. A motion of none hides the shape's rows.

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
fn an_aim_sheet_shows_the_rows_its_base_and_motion_use() {
    let name = "aim-sheet";
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
        const inSheet=role=>{
            const p=node("card","Clip inputs").bounds;
            const inside=n=>n.bounds.x>=p.x&&n.bounds.x<p.x+p.width;
            return app.snapshot().findAll({role}).filter(inside).map(n=>n.label);
        };
        const inRow=(row,role,label)=>{
            const r=node("row",row).bounds;
            const found=app.snapshot().findAll({role,label}).find(n=>n.bounds.y>=r.y&&n.bounds.y<r.y+r.height);
            if(!found) throw new Error(`no ${role} ${label} in ${row}`);
            return found;
        };
        const settle=()=>app.frames(16,{waitMs:60});
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        // Aim's own Wave: the search matches the form's name, not Chase's.
        app.type(node("input","Search presets…"),"aim"); app.frames(2);
        app.click(node("row","Wave"));
        until("aim inputs",s=>s.find({role:"row",label:"Motion"}));
        settle();
        const shaped=inSheet("row");
        const sliders=inSheet("slider");
        const texts=inSheet("text");
        app.click(inRow("Motion","select","Shape"));
        app.click(node("button","None"));
        until("motion none",s=>s.find({role:"select",label:"None"}));
        settle();
        const still=inSheet("row");
        ({shaped,sliders,texts,still})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    let rows = |key: &str| -> Vec<&str> {
        let aim = [
            "Blend",
            "Base",
            "Direction",
            "Point",
            "Fan",
            "Axis",
            "Motion",
            "Shape",
            "Size",
            "Every",
            "Spread",
            "Speed",
            "Alpha",
        ];
        labels(&out[key])
            .into_iter()
            .filter(|label| aim.contains(label))
            .collect()
    };
    assert_eq!(
        rows("shaped"),
        [
            "Base",
            "Direction",
            "Fan",
            "Axis",
            "Motion",
            "Shape",
            "Size",
            "Every",
            "Spread"
        ],
        "a direction base with a shape: {out}"
    );
    assert!(
        labels(&out["sliders"]).contains(&"Direction: Turn = 0")
            && labels(&out["sliders"]).contains(&"Direction: Tilt = -40"),
        "direction is turn and tilt: {out}"
    );
    assert!(
        labels(&out["texts"]).contains(&"U 0.00 · V 0.77 · Z −0.64"),
        "the stored vector shows under turn and tilt: {out}"
    );
    assert_eq!(
        rows("still"),
        ["Base", "Direction", "Fan", "Axis", "Motion"],
        "a motion of none hides the shape's rows: {out}"
    );

    let score = stored(name);
    let clips = score["clips"].as_object().unwrap();
    assert_eq!(clips.len(), 1, "{score}");
    let clip = clips.values().next().unwrap();
    assert_eq!(clip["graph"], "aim@1");
    assert_eq!(clip["blend_mode"], "replace", "{clip}");
    assert_eq!(
        clip["inputs"]["motion"],
        serde_json::json!({"type": "choice", "value": "none"})
    );
    assert_eq!(
        clip["inputs"]["shape"],
        serde_json::json!({"type": "choice", "value": "swing_up_down"}),
        "a hidden input keeps its value"
    );
}

/// The axis rows offer the Mirror control of the old mapping editor: Off,
/// Left–right, Front–back, Up–down and Custom plane, with the plane's normal
/// for a custom plane and its offset whenever there is a mirror. Only a
/// spatial axis along a line takes a mirror: order and random hide the row,
/// and picking Random drops the mirror. Spread shows in degrees.
#[test]
fn an_aim_axis_offers_the_mirror_control() {
    let name = "aim-axis-mirror";
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
        const inSheet=role=>{
            const p=node("card","Clip inputs").bounds;
            const inside=n=>n.bounds.x>=p.x&&n.bounds.x<p.x+p.width;
            return app.snapshot().findAll({role}).filter(inside).map(n=>n.label);
        };
        const fields=()=>inSheet("slider").concat(inSheet("input"));
        const settle=()=>app.frames(16,{waitMs:60});
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.type(node("input","Search presets…"),"aim"); app.frames(2);
        app.click(node("row","Wave"));
        until("aim inputs",s=>s.find({role:"row",label:"Motion"}));
        settle();
        const order=inSheet("text");
        const values=fields().concat(inSheet("text"));
        app.click(node("select","Order"));
        until("axis menu",s=>s.find({role:"button",label:"X"}));
        app.click(node("button","X"));
        until("x axis",s=>s.find({role:"select",label:"X"}));
        settle();
        const x=inSheet("text");
        app.click(node("select","Off"));
        until("mirror menu",s=>s.find({role:"button",label:"Custom plane"}));
        const mirrors=app.snapshot().findAll({role:"button"}).map(n=>n.label)
            .filter(l=>["Off","Left–right","Front–back","Up–down","Custom plane"].includes(l));
        app.click(node("button","Custom plane"));
        until("custom",s=>s.find({role:"select",label:"Custom plane"}));
        settle();
        const custom=inSheet("text");
        const customFields=fields();
        app.click(node("select","Custom plane"));
        until("mirror menu",s=>s.find({role:"button",label:"Front–back"}));
        app.click(node("button","Front–back"));
        until("front-back",s=>s.find({role:"select",label:"Front–back"}));
        settle();
        const fixed=inSheet("text");
        const fixedFields=fields();
        app.click(node("select","X"));
        until("axis menu",s=>s.find({role:"button",label:"Random"}));
        app.click(node("button","Random"));
        until("random",s=>s.find({role:"select",label:"Random"}));
        settle();
        const random=inSheet("text");
        ({order,values,x,mirrors,custom,customFields,fixed,fixedFields,random})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    let has = |key: &str, label: &str| labels(&out[key]).contains(&label);
    let field = |key: &str, prefix: &str| labels(&out[key]).iter().any(|l| l.starts_with(prefix));
    assert!(!has("order", "Mirror"), "order takes no mirror: {out}");
    assert!(has("x", "Mirror"), "an x axis has a Mirror row: {out}");
    assert!(!has("x", "Offset"), "no offset without a mirror: {out}");
    assert_eq!(
        labels(&out["mirrors"]),
        ["Off", "Left–right", "Front–back", "Up–down", "Custom plane"],
        "the old mapping editor's mirrors: {out}"
    );
    assert!(
        has("custom", "Normal · U, V, Z") && has("custom", "Offset"),
        "a custom plane shows its normal and offset: {out}"
    );
    assert!(
        field("customFields", "Axis: Mirror U") && field("customFields", "Axis: Mirror offset"),
        "{out}"
    );
    assert!(
        !has("fixed", "Normal · U, V, Z") && has("fixed", "Offset"),
        "a fixed plane shows only its offset: {out}"
    );
    assert!(field("fixedFields", "Axis: Mirror offset"), "{out}");
    assert!(
        labels(&out["values"]).iter().any(|l| l.contains("216")),
        "Wave's spread is 216 degrees: {out}"
    );
    assert!(
        !has("random", "Mirror"),
        "a random axis has no Mirror row: {out}"
    );

    let score = stored(name);
    let clip = score["clips"].as_object().unwrap().values().next().unwrap();
    let axis = &clip["inputs"]["axis"]["value"];
    assert_eq!(axis["source"]["kind"], "random", "{clip}");
    assert!(
        axis.get("mirror").is_none(),
        "Random drops the mirror: {clip}"
    );
    assert_eq!(
        clip["inputs"]["spread"],
        serde_json::json!({"type": "number", "value": 216.0})
    );
}
