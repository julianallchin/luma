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
        .with_graph_score(support::score(serde_json::json!({})))
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
        "Color", "Axis", "Every", "Travel", "Width", "Shape", "Path", "Boundary",
    ];
    let inputs: Vec<&str> = out["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|label| order.contains(label))
        .collect();
    assert_eq!(inputs, order, "the sheet shows the form's inputs in order");
    assert!(
        !out["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "Alpha"),
        "alpha is edited on the timeline, not in the sheet: {out}"
    );
    assert_eq!(
        out["sheet"], true,
        "a double-click keeps the inputs up: {out}"
    );

    let score = stored("clip-forms-picker");
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
fn a_curve_input_offers_its_curves_and_custom_opens_the_editor() {
    let mut harness = Fixture::new("clip-forms-width-curves", 20, vec![])
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
        until("form inputs",s=>s.find({role:"row",label:"Width"}));
        app.click(inRow("Width","select","Fixed"));
        app.click(node("button","↗ Over time"));
        // A flat curve is no preset: Custom, with its editor open.
        until("curve editor",s=>s.find({role:"card",label:"Envelope curve"}));
        settle();
        const editorPresets=["Hard","Soft","Triangle"].filter(l=>app.snapshot().find({role:"button",label:l}));
        const chip=inRow("Width","select","Custom");
        app.click(chip);
        const grid=node("card","Presets").bounds;
        const cells=app.snapshot().findAll({role:"button"})
            .filter(n=>n.bounds.x>=grid.x&&n.bounds.y>=grid.y&&n.bounds.x<grid.x+grid.width&&n.bounds.y<grid.y+grid.height)
            .map(n=>({label:n.label,y:n.bounds.y,w:n.bounds.width}));
        app.click(node("button","Swell"));
        until("picked",s=>s.find({role:"select",label:"Swell"}));
        settle();
        const closed=!app.snapshot().find({role:"card",label:"Presets"});
        const editorGone=!app.snapshot().find({role:"card",label:"Envelope curve"});
        ({editorPresets,grid,chip:chip.bounds,cells,closed,editorGone})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    assert_eq!(
        out["editorGone"], true,
        "picking a preset hides it again: {out}"
    );
    assert_eq!(
        out["editorPresets"],
        serde_json::json!([]),
        "the editor drops its own presets beside the picker: {out}"
    );
    let offered: Vec<&str> = luma_patterns::presets()
        .curves_for("width")
        .map(|curve| curve.name.as_str())
        .chain(["Custom"])
        .collect();
    // The popover lies over the editor; keep only its own thumbnails.
    let cells: Vec<&serde_json::Value> = out["cells"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| offered.contains(&c["label"].as_str().unwrap()))
        .collect();
    let labels: Vec<&str> = cells.iter().map(|c| c["label"].as_str().unwrap()).collect();
    assert_eq!(labels, offered, "the input's curves, then Custom: {out}");
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
    assert_eq!(rows, [4, 4], "{out}");
    assert!(
        out["chip"]["width"].as_f64().unwrap() > 250.,
        "the chip spans the column like every value control: {out}"
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

    let score = stored("clip-forms-width-curves");
    let swell = luma_patterns::presets()
        .curves_for("width")
        .find(|curve| curve.name == "Swell")
        .unwrap();
    assert_eq!(
        score["clips"]["form-clip"]["inputs"]["width"],
        serde_json::json!({"type": "time", "value": swell.curve}),
        "the pick stores the curve: {}",
        score["clips"]["form-clip"]["inputs"]["width"]
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
        const hex=()=>app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith("Stop color hex = "));
        const opacity=()=>app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith("Stop color opacity = "));
        const swatch=()=>node("button","Stop color swatch");
        const open=()=>{if(!hex())app.click(swatch());until("picker",()=>hex());};
        const shut=()=>{if(hex()){app.click(swatch());until("picker closed",()=>!hex());}};
        const settle=()=>app.frames(16,{waitMs:60});
        app.click(node("card","Color over time"));
        until("gradient",s=>s.find({role:"card",label:"graph-gradient bar"}));
        const before=stops().length;
        app.click(node("card","graph-gradient bar"));
        until("added",()=>stops().length===before+1);
        settle();
        const added=stops().length;
        const row=!hex()&&!opacity();
        open();
        const firstHex=hex().label, firstOpacity=opacity().label;
        shut();
        app.drag(stops()[1],{dx:0,dy:90},{steps:6});
        until("dragged off",()=>stops().length===before);
        settle();
        const removed=stops().length;
        open();
        app.click(hex());app.key("secondary-a backspace");app.type(hex(),"#00ff00");app.key("enter");
        until("hex",()=>hex().label==="Stop color hex = #00FF00");
        shut();
        settle();
        app.click(node("select","Custom"));
        app.click(node("button","Fire"));
        until("fire",s=>s.find({role:"select",label:"Fire"}));
        settle();
        ({before,added,row,firstHex,firstOpacity,removed})
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
    assert_eq!(out["firstHex"], "Stop color hex = #9964A2", "{out}");
    assert_eq!(
        out["row"], true,
        "hex and opacity live in the picker: {out}"
    );
    assert_eq!(out["firstOpacity"], "Stop color opacity = 100", "{out}");

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

#[test]
fn an_envelope_point_dragged_outside_the_editor_keeps_following_and_clamps() {
    let mut harness = Fixture::new("clip-forms-envelope-drag", 20, vec![])
        .with_graph_score(support::preset_score("Chase"))
        .with_rig()
        .window(1400., 1400.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const reveal=target=>{
            const p=node("card","Clip inputs").bounds, b=target.bounds;
            if(b.y<p.y+70 || b.y+b.height>p.y+p.height-160) {
                app.scroll({x:p.x+p.width/2,y:p.y+p.height/2},{dy:(p.y+140)-b.y,steps:5});
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
        until("form inputs",s=>s.find({role:"row",label:"Width"}));
        app.click(inRow("Width","select","Fixed"));
        app.click(node("button","↗ Over time"));
        until("curve editor",s=>s.find({role:"card",label:"Envelope curve"}));
        settle();
        const box=node("card","Envelope curve").bounds;
        const end=node("slider","Envelope anchor 2");
        // Down and to the left, far past the box, and released out there.
        app.drag(end,{dx:-300,dy:box.height*3},{steps:8});
        settle();
        ({box})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let score = stored("clip-forms-envelope-drag");
    let width = &score["clips"]["form-clip"]["inputs"]["width"];
    assert_eq!(width["type"], "time", "{width}");
    let points = width["value"]["points"].as_array().unwrap();
    let last = points.last().unwrap();
    // The end point keeps its x and follows the pointer to the bottom.
    assert_eq!(last[0].as_f64(), Some(1.0), "{width}");
    assert_eq!(last[1].as_f64(), Some(0.0), "{width}");
}

#[test]
fn a_click_elsewhere_blurs_a_field_and_commits_its_value() {
    let mut harness = Fixture::new("clip-forms-blur", 20, vec![])
        .with_graph_score(support::preset_score("Chase"))
        .with_rig()
        .window(1400., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const field=()=>app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith("Width = "));
        app.click(node("card","Chase"));
        until("width",()=>field());
        app.click(field());
        until("focused",()=>field().focused);
        app.key("secondary-a backspace");
        app.type(field(),"50");
        app.frames(4);
        const typing=field().focused;
        // A press on the sheet's own text, nowhere near the field.
        app.click(node("text","Form"));
        until("blurred",()=>!field().focused);
        app.frames(16,{waitMs:60});
        ({typing,after:field().label,focused:field().focused})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = &result.result;
    assert_eq!(out["typing"], true, "{out}");
    assert_eq!(out["focused"], false, "{out}");
    assert_eq!(
        out["after"], "Width = 50",
        "width reads as a percent: {out}"
    );
    let score = stored("clip-forms-blur");
    assert_eq!(
        score["clips"]["form-clip"]["inputs"]["width"],
        serde_json::json!({"type": "proportion", "value": 0.5}),
        "blur commits the typed value"
    );
}

#[test]
fn every_sheet_row_has_one_shape() {
    let mut harness = Fixture::new("clip-forms-rows", 20, vec![])
        .with_graph_score(support::preset_score("Pulse"))
        .with_rig()
        .window(1400., 1400.)
        .open(Mode::Headless);
    let result = harness.exec(
        &support::script(
            r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        app.click(node("card","Sparkle"));
        until("rows",s=>s.find({role:"row",label:"Grain"}));
        app.frames(8,{waitMs:40});
        const snap=app.snapshot();
        const sheet=snap.find({role:"card",label:"Clip inputs"}).bounds;
        const rows=snap.findAll({role:"row"}).filter(r=>r.bounds.x>=sheet.x&&r.bounds.x<sheet.x+sheet.width);
        const inside=(r,n)=>n.bounds.y>=r.bounds.y&&n.bounds.y<r.bounds.y+r.bounds.height&&n.bounds.x>=r.bounds.x;
        const controls=snap.findAll(n=>n.role==="select"||n.role==="input");
        const out=rows.map(r=>{
            const own=controls.filter(n=>inside(r,n));
            const head=own.filter(n=>n.bounds.y<r.bounds.y+24);
            const body=own.filter(n=>n.bounds.y>=r.bounds.y+24);
            return {label:r.label,
                head:head.map(n=>({label:n.label,w:n.bounds.width})),
                body:body.map(n=>({label:n.label,w:n.bounds.width,h:n.bounds.height,y:n.bounds.y-r.bounds.y}))};
        });
        ({rows:out})
    "#,
        ),
        Duration::from_secs(90),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let rows = result.result["rows"].as_array().unwrap().clone();
    let labels: Vec<&str> = rows.iter().map(|r| r["label"].as_str().unwrap()).collect();
    for label in &labels {
        let first = label.chars().next().unwrap();
        assert!(
            first.is_uppercase(),
            "row labels are sentence case: {labels:?}"
        );
    }
    for want in ["Blend", "Selection", "Brightness", "Grain"] {
        assert!(labels.contains(&want), "no {want} row: {labels:?}");
    }
    // Mode menus in the header all have one width.
    let modes: Vec<f64> = rows
        .iter()
        .flat_map(|r| r["head"].as_array().unwrap().clone())
        .map(|c| c["w"].as_f64().unwrap())
        .collect();
    assert!(modes.len() >= 3, "{rows:?}");
    assert!(
        modes.windows(2).all(|w| (w[0] - w[1]).abs() < 1.),
        "mode widths {modes:?}"
    );
    // Value controls span the column, one control height, one gap under the
    // header.
    let bodies: Vec<serde_json::Value> = rows
        .iter()
        .filter(|r| r["label"] != "Selection")
        .flat_map(|r| r["body"].as_array().unwrap().first().cloned())
        .collect();
    let widths: Vec<f64> = bodies.iter().map(|c| c["w"].as_f64().unwrap()).collect();
    assert!(bodies.len() >= 5, "{rows:?}");
    assert!(
        widths.windows(2).all(|w| (w[0] - w[1]).abs() < 1.),
        "value widths {widths:?} in {rows:?}"
    );
    let tops: Vec<f64> = bodies.iter().map(|c| c["y"].as_f64().unwrap()).collect();
    assert!(
        tops.windows(2).all(|w| (w[0] - w[1]).abs() < 1.),
        "label gaps {tops:?}"
    );
}
