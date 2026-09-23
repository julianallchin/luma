//! Form clips from the outside: the picker offers shipped presets only and
//! places a form clip, and the sheet edits a form's inputs, including a
//! promotion to a curve over time and back.

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

#[test]
fn the_picker_lists_presets_and_places_a_form_clip() {
    let mut harness = Fixture::new("clip-forms-picker", 20, vec![])
        .with_graph_score(support::score(serde_json::json!({}), serde_json::json!({})))
        .with_rig()
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const rows=()=>app.snapshot().findAll({role:"row"}).map(n=>n.label);
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        const before=rows();
        app.click(node("row","Lane 0"),{button:"right"});
        node("card","Insert pattern dialog");
        const offered=rows().filter(label=>!before.includes(label));
        app.type(node("input","Search patterns…"),"chase"); app.frames(2);
        const shown=rows().filter(label=>!before.includes(label));
        app.key("enter");
        app.click(node("card","Chase"));
        until("form inputs",s=>s.find({role:"row",label:"Travel"}));
        const inputs=rows();
        app.click(node("card","Chase"),{count:2});
        app.frames(12,{waitMs:40});
        ({offered,shown,inputs,
          graph:!!app.snapshot().find({role:"card",label:"Graph workspace"}),
          sheet:!!app.snapshot().find({role:"card",label:"Clip inputs"})})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    let offered: Vec<&str> = out["offered"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(offered.first(), Some(&"Wash"), "{out}");
    assert!(offered.contains(&"Chase"), "{out}");
    assert!(
        !offered.contains(&"Beat chase"),
        "the picker lists presets only: {out}"
    );
    assert_eq!(out["shown"][0], "Chase", "{out}");
    assert!(
        out["shown"].as_array().unwrap().iter().any(|v| v == "Wave"),
        "a search matches the form's name: {out}"
    );
    let order = [
        "Color", "Axis", "Every", "Travel", "Width", "Shape", "Path", "Alpha", "Boundary",
    ];
    let inputs: Vec<&str> = out["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|label| order.contains(label))
        .collect();
    assert_eq!(inputs, order, "the sheet shows the form's inputs in order");
    assert_eq!(out["graph"], false, "a form clip opens no graph tab: {out}");
    assert_eq!(
        out["sheet"], true,
        "a double-click keeps the inputs up: {out}"
    );

    let score = stored("clip-forms-picker");
    assert!(score["definitions"].as_object().unwrap().is_empty());
    let clips = score["clips"].as_object().unwrap();
    assert_eq!(clips.len(), 1);
    let clip = clips.values().next().unwrap();
    assert_eq!(clip["graph"], "color.chase@1");
    let chase = luma_patterns::presets().preset("Chase").unwrap();
    assert_eq!(
        clip["inputs"],
        serde_json::to_value(&chase.inputs).unwrap(),
        "a placed clip copies every value of its preset"
    );
}

#[test]
fn the_sheet_edits_a_choice_and_promotes_an_input_to_a_curve_and_back() {
    let mut harness = Fixture::new("clip-forms-sheet", 20, vec![])
        .with_graph_score(support::preset_score("Chase"))
        .with_rig()
        .window(1400., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const reveal=target=>{
            const p=node("card","Clip inputs").bounds, b=target.bounds;
            if(b.y<p.y+70 || b.y+b.height>p.y+p.height-12) {
                app.scroll({x:p.x+p.width/2,y:p.y+p.height/2},{dy:(p.y+p.height/2)-b.y,steps:5});
                app.frames(3);
            }
        };
        // The control of `role` and `label` inside the row named `row`.
        const inRow=(row,role,label)=>{
            reveal(node("row",row));
            const r=node("row",row).bounds;
            const found=app.snapshot().findAll({role,label}).find(n=>n.bounds.y>=r.y&&n.bounds.y<r.y+r.height);
            if(!found) throw new Error(`no ${role} ${label} in ${row}`);
            return found;
        };
        const settle=()=>app.frames(16,{waitMs:60});
        app.click(node("card","Chase"));
        until("form inputs",s=>s.find({role:"row",label:"Shape"}));

        app.click(inRow("Shape","select","Hard"));
        app.click(node("button","Comet"));
        until("comet",s=>s.find({role:"select",label:"Comet"}));
        settle();

        app.click(inRow("Every","select","Plain"));
        app.click(node("button","↗ Over time"));
        until("curve editor",s=>s.find({role:"card",label:"Envelope curve"}));
        const promoted=!!inRow("Every","select","↗ Over time");
        settle();

        app.click(inRow("Every","select","Custom"));
        app.click(node("button","Swell"));
        until("swell",s=>s.find({role:"select",label:"Swell"}));
        settle();

        app.click(inRow("Every","select","↗ Over time"));
        app.click(node("button","Plain"));
        until("plain again",s=>!s.find({role:"card",label:"Envelope curve"}));
        const every=app.snapshot().findAll({role:"input"}).map(n=>n.label).filter(l=>l.startsWith("Every"));
        settle();
        ({promoted,every})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    assert_eq!(result.result["promoted"], true, "{}", result.result);
    assert_eq!(
        result.result["every"],
        serde_json::json!(["Every = 0.0625"]),
        "back to plain takes the curve's first value: {}",
        result.result
    );

    let score = stored("clip-forms-sheet");
    let clip = &score["clips"]["form-clip"];
    assert_eq!(clip["graph"], "color.chase@1");
    let comet = luma_patterns::shape_presets()
        .into_iter()
        .find(|(label, _)| *label == "Comet")
        .unwrap()
        .1;
    assert_eq!(
        clip["inputs"]["shape"],
        serde_json::to_value(&comet).unwrap(),
        "the shape select stores the named shape"
    );
    assert_eq!(
        clip["inputs"]["every"],
        serde_json::json!({"type": "beats", "value": 0.0625}),
        "the promotion went to a curve and came back plain"
    );
}
