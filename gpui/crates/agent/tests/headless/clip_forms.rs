//! Form clips from the outside: the picker offers shipped presets only and
//! places a form clip, and the sheet edits a form's inputs, including a
//! promotion to a curve over time, a pick from the curve thumbnails, and back
//! to a fixed value.

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
    let curves: Vec<&str> = luma_patterns::presets()
        .curves_for("every")
        .map(|curve| curve.name.as_str())
        .collect();
    let result = harness.exec(
        &(format!("const CURVES={};", serde_json::json!(curves))
            + &support::script(
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
        const shapeEditorHidden=!app.snapshot().find({role:"card",label:"Envelope curve"});
        app.click(inRow("Shape","select","Comet"));
        app.click(node("button","Custom"));
        until("shape editor",s=>s.find({role:"card",label:"Envelope curve"}));
        const shapeEditor=!!inRow("Shape","select","Comet");
        app.click(inRow("Shape","select","Comet"));
        app.click(node("button","Comet"));
        until("shape editor hidden",s=>!s.find({role:"card",label:"Envelope curve"}));
        settle();

        const modes=()=>app.snapshot().findAll({role:"button"}).map(n=>n.label);
        app.click(inRow("Every","select","Fixed"));
        const offered=modes().filter(l=>l==="Fixed"||l.startsWith("↗"));
        app.click(node("button","↗ Over time"));
        until("curve editor",s=>s.find({role:"card",label:"Envelope curve"}));
        const promoted=!!inRow("Every","select","↗ Over time");
        settle();

        app.click(inRow("Every","select","Custom"));
        node("button","Swell");
        const thumbs=modes().filter(l=>CURVES.includes(l));
        app.click(node("button","Swell"));
        until("swell",s=>s.find({role:"select",label:"Swell"}));
        settle();
        const closed=!app.snapshot().find({role:"button",label:"Swell"});
        const presetHidesEditor=!app.snapshot().find({role:"card",label:"Envelope curve"});

        app.click(inRow("Every","select","↗ Over time"));
        app.click(node("button","Fixed"));
        until("fixed again",s=>s.find({role:"select",label:"Fixed"}));
        const fixed=!!inRow("Every","select","Fixed");
        const every=app.snapshot().findAll({role:"input"}).map(n=>n.label).filter(l=>l.startsWith("Every"));
        settle();
        ({shapeEditorHidden,shapeEditor,offered,promoted,thumbs,closed,presetHidesEditor,fixed,every})
    "#,
            )),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    assert_eq!(
        out["offered"],
        serde_json::json!(["Fixed", "↗ Over time", "↗ Stamped beats"]),
        "every's mode menu: {out}"
    );
    assert_eq!(out["promoted"], true, "{out}");
    assert_eq!(
        out["thumbs"],
        serde_json::json!(curves),
        "the popover shows one thumbnail per curve preset: {out}"
    );
    assert_eq!(out["closed"], true, "a pick closes the popover: {out}");
    assert_eq!(out["shapeEditorHidden"], true, "{out}");
    assert_eq!(
        out["shapeEditor"], true,
        "Custom opens the shape's editor: {out}"
    );
    assert_eq!(out["presetHidesEditor"], true, "{out}");
    assert_eq!(out["fixed"], true, "{out}");
    assert_eq!(
        out["every"],
        serde_json::json!(["Every = 0.0625"]),
        "back to fixed takes the curve's first value: {out}"
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

#[test]
fn alpha_offers_its_own_curves_and_custom_opens_the_editor() {
    let mut harness = Fixture::new("clip-forms-alpha-curves", 20, vec![])
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
        const inRow=(row,role,label)=>{
            reveal(node("row",row));
            const r=node("row",row).bounds;
            const found=app.snapshot().findAll({role,label}).find(n=>n.bounds.y>=r.y&&n.bounds.y<r.y+r.height);
            if(!found) throw new Error(`no ${role} ${label} in ${row}`);
            return found;
        };
        const settle=()=>app.frames(16,{waitMs:60});
        app.click(node("card","Chase"));
        until("form inputs",s=>s.find({role:"row",label:"Alpha"}));
        app.click(inRow("Alpha","select","Fixed"));
        app.click(node("button","↗ Over time"));
        until("full",s=>s.find({role:"select",label:"Full"}));
        settle();
        // A preset keeps the editor out of the way.
        const hidden=!app.snapshot().find({role:"card",label:"Envelope curve"});
        app.click(inRow("Alpha","select","Full"));
        app.click(node("button","Custom"));
        until("curve editor",s=>s.find({role:"card",label:"Envelope curve"}));
        settle();
        const editorPresets=["Hard","Soft","Triangle"].filter(l=>app.snapshot().find({role:"button",label:l}));
        const chip=inRow("Alpha","select","Full");
        app.click(chip);
        const grid=node("card","Presets").bounds;
        const cells=app.snapshot().findAll({role:"button"})
            .filter(n=>n.bounds.x>=grid.x&&n.bounds.y>=grid.y&&n.bounds.x<grid.x+grid.width&&n.bounds.y<grid.y+grid.height)
            .map(n=>({label:n.label,y:n.bounds.y,w:n.bounds.width}));
        app.click(node("button","Fade in-out"));
        until("picked",s=>s.find({role:"select",label:"Fade in-out"}));
        settle();
        const closed=!app.snapshot().find({role:"card",label:"Presets"});
        const editorGone=!app.snapshot().find({role:"card",label:"Envelope curve"});
        ({hidden,editorPresets,grid,chip:chip.bounds,cells,closed,editorGone})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    assert_eq!(out["hidden"], true, "a preset hides the editor: {out}");
    assert_eq!(
        out["editorGone"], true,
        "picking a preset hides it again: {out}"
    );
    assert_eq!(
        out["editorPresets"],
        serde_json::json!([]),
        "the editor drops its own presets beside the picker: {out}"
    );
    let alpha: Vec<&str> = luma_patterns::presets()
        .curves_for("alpha")
        .map(|curve| curve.name.as_str())
        .chain(["Custom"])
        .collect();
    // The popover lies over the editor; keep only its own thumbnails.
    let cells: Vec<&serde_json::Value> = out["cells"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| alpha.contains(&c["label"].as_str().unwrap()))
        .collect();
    let labels: Vec<&str> = cells.iter().map(|c| c["label"].as_str().unwrap()).collect();
    assert_eq!(
        labels, alpha,
        "alpha offers its own curves, then Custom: {out}"
    );
    // A tidy grid: rows as full as the first, the last no longer.
    let mut rows: Vec<usize> = Vec::new();
    let mut last = f64::NAN;
    for cell in &cells {
        let y = cell["y"].as_f64().unwrap();
        if y != last {
            rows.push(0);
            last = y;
        }
        *rows.last_mut().unwrap() += 1;
    }
    assert_eq!(rows, [5, 4], "{out}");
    assert!(
        out["chip"]["width"].as_f64().unwrap() < 160.,
        "the chip is as wide as its content: {out}"
    );
    assert!(
        cells.iter().all(|c| c["w"].as_f64().unwrap() >= 40.),
        "{out}"
    );
    let (grid, chip) = (&out["grid"], &out["chip"]);
    assert!(
        grid["y"].as_f64().unwrap()
            >= chip["y"].as_f64().unwrap() + chip["height"].as_f64().unwrap(),
        "the popover hangs under the chip: {out}"
    );
    assert_eq!(out["closed"], true, "a pick closes the popover: {out}");

    let score = stored("clip-forms-alpha-curves");
    let fade = luma_patterns::presets()
        .curves_for("alpha")
        .find(|curve| curve.name == "Fade in-out")
        .unwrap();
    assert_eq!(
        score["clips"]["form-clip"]["inputs"]["alpha"],
        serde_json::json!({"type": "time", "value": fade.curve}),
        "the pick stores the alpha curve: {}",
        score["clips"]["form-clip"]["inputs"]["alpha"]
    );
}

#[test]
fn the_gradient_editor_adds_drags_off_types_hex_and_picks_a_preset() {
    let mut harness = Fixture::new("clip-forms-gradient", 20, vec![])
        .with_graph_score(support::preset_score("Color fade"))
        .with_rig()
        .window(1400., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r##"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const stops=()=>app.snapshot().findAll({role:"slider"}).filter(n=>n.label.startsWith("graph-gradient:stop:"));
        const hex=()=>app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith("Stop hex = "));
        const settle=()=>app.frames(16,{waitMs:60});
        app.click(node("card","Color over time"));
        until("gradient",s=>s.find({role:"card",label:"graph-gradient bar"}));
        const before=stops().length;
        app.click(node("card","graph-gradient bar"));
        until("added",()=>stops().length===before+1);
        settle();
        const added=stops().length;
        const firstHex=hex().label;
        app.drag(stops()[1],{dx:0,dy:90},{steps:6});
        until("dragged off",()=>stops().length===before);
        settle();
        const removed=stops().length;
        app.click(hex());app.key("secondary-a backspace");app.type(hex(),"#00ff00");app.key("enter");
        until("hex",()=>hex().label==="Stop hex = #00FF00");
        settle();
        app.click(node("select","Custom"));
        app.click(node("button","Fire"));
        until("fire",s=>s.find({role:"select",label:"Fire"}));
        settle();
        ({before,added,firstHex,removed})
    "##,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    assert_eq!(out["before"], 2, "{out}");
    assert_eq!(out["added"], 3, "a click on the bar adds a stop: {out}");
    assert_eq!(out["removed"], 2, "a stop dragged off goes: {out}");
    // The new stop is selected and carries the bar's color there.
    assert_eq!(out["firstHex"], "Stop hex = #9964A2", "{out}");

    let score = stored("clip-forms-gradient");
    let fire = luma_patterns::presets()
        .gradients
        .iter()
        .find(|preset| preset.name == "Fire")
        .unwrap();
    let colors: luma_patterns::Value =
        serde_json::from_value(score["clips"]["form-clip"]["inputs"]["colors"].clone()).unwrap();
    let luma_patterns::Value::Gradient(colors) = colors else {
        panic!("{colors:?}")
    };
    assert_eq!(colors.stops.len(), fire.gradient.stops.len());
    for (a, b) in colors.stops.iter().zip(&fire.gradient.stops) {
        assert!(
            (a.t - b.t).abs() < 1e-4
                && a.color
                    .iter()
                    .zip(b.color)
                    .all(|(x, y)| (x - y).abs() < 3e-3)
        );
    }
}
