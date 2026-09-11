use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn edit_custom_mapping_and_mirror_without_losing_clip_values() {
    let mut harness = Fixture::new("mapping-vector-mirror", 20, vec![])
        .with_graph_score(serde_json::json!({"version":7,"definitions":{},"clips":{}}))
        .with_rig()
        .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const reveal=target=>{
            const snapshot=app.snapshot();
            const pane=snapshot.find({role:"card",label:"Node inspector"}) || snapshot.find({role:"card",label:"Clip inputs"});
            if(pane && target.bounds.x>=pane.bounds.x && target.bounds.x<pane.bounds.x+pane.bounds.width) {
                const p=pane.bounds, b=target.bounds;
                if(b.y<p.y+70 || b.y+b.height>p.y+p.height-12) {
                    app.scroll({x:p.x+p.width/2,y:p.y+p.height/2},{dy:(p.y+p.height/2)-b.y,steps:5});
                    app.frames(3);
                }
            }
        };
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.click(node("row","Lane 0"),{button:"right"});
        app.type(node("input","Search patterns…"),"chase");
        app.frames(2); app.key("enter");
        app.click(node("card","Beat chase"));
        app.click(node("select","Up (Z+)"));
        app.click(node("button","Custom vector"));
        const field=name=>app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith(name+" = "));
        const retype=(name,value)=>{
            until(name,s=>s.findAll({role:"input"}).some(n=>n.label.startsWith(name+" = ")));
            reveal(field(name)); app.click(field(name)); app.key("cmd-a backspace");
            app.type(field(name),value,{restale:"match"}); app.key("enter");
            app.frames(6,{waitMs:80});
        };
        retype("Mapping: Z","2");
        app.click(node("select","Off"));
        app.click(node("button","Left–right"));
        retype("Mapping: Mirror offset","0.25");
        reveal(node("select","Left–right"));
        app.click(node("select","Left–right"));
        app.click(node("button","Custom plane"));
        retype("Mapping: Mirror Z","1");
        reveal(node("button","Mapping: Reverse"));
        app.click(node("button","Mapping: Reverse"));
        app.frames(12,{waitMs:80});
        app.click(node("card","Beat chase"),{count:2});
        app.click(node("card","Resolve Mapping"));
        app.click(node("button","Edit Input Mapping"));
        reveal(node("select","Up (Z+)"));
        app.click(node("select","Up (Z+)"));
        app.click(node("button","Custom vector"));
        retype("Value: Z","3");
        app.click(node("card","Mapping"));
        app.key("secondary-z");
        until("undo mapping",s=>s.find({role:"input",label:"Value: Z = 1"}));
        app.key("secondary-shift-z");
        until("redo mapping",s=>s.find({role:"input",label:"Value: Z = 3"}));
        app.frames(16,{waitMs:80});
        ({fields:app.snapshot().findAll({role:"input"}).map(n=>n.label)})
    "#), Duration::from_secs(60));
    assert_eq!(result.error, None, "{}", result.stdout);
    let root = support::config_dir("mapping-vector-mirror");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool =
                sqlx::SqlitePool::connect(&format!("sqlite:{}", root.join("luma.db").display()))
                    .await
                    .unwrap();
            let source: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: serde_json::Value = serde_json::from_str(&source).unwrap();
            let clip = score["clips"].as_object().unwrap().values().next().unwrap();
            let mapping = &clip["inputs"]["mapping"]["value"];
            assert_eq!(
                serde_json::from_value::<[f64; 3]>(mapping["source"]["direction"].clone()).unwrap(),
                [1., 0., 2.]
            );
            assert_eq!(
                serde_json::from_value::<[f64; 3]>(mapping["mirror"]["normal"].clone()).unwrap(),
                [1., 0., 1.]
            );
            assert_eq!(mapping["mirror"]["offset"], 0.25);
            assert_eq!(mapping["reverse"], true);
            let definition = &score["definitions"][clip["graph"].as_str().unwrap()];
            assert_eq!(
                serde_json::from_value::<[f64; 3]>(
                    definition["inputs"]["mapping"]["default"]["value"]["source"]["direction"]
                        .clone()
                )
                .unwrap(),
                [1., 0., 3.]
            );
            pool.close().await;
        });
}

#[test]
fn edit_gradient_stops_in_a_canonical_graph() {
    let mut harness = Fixture::new("lighting-gradient-edit", 20, vec![])
        .with_graph_score(serde_json::json!({"version":7,"definitions":{},"clips":{}}))
        .with_rig()
        .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.click(app.snapshot().find({role:"row",label:"Lane 0"}),{button:"right"});
        until("search",s=>s.find({role:"input",label:"Search patterns…"}));
        const field=app.snapshot().find({role:"input",label:"Search patterns…"});
        app.type(field,"color fade"); app.frames(2); app.key("enter");
        until("Color fade clip",s=>s.find({role:"card",label:"Color fade"}));
        const clip=app.snapshot().find({role:"card",label:"Color fade"});
        app.click(clip);
        app.click(app.snapshot().find({role:"card",label:clip.label}),{count:2});
        until("graph",s=>s.find({role:"card",label:"Graph workspace"}));
        // Connected parameters are edited at their exposed Input.
        app.click(app.snapshot().find({role:"card",label:"Gradient"}));
        until("gradient stops",s=>s.findAll({role:"slider"}).find(n=>n.label.startsWith("graph-gradient:stop:0")));
        const stop=app.snapshot().findAll({role:"slider"}).find(n=>n.label.startsWith("graph-gradient:stop:0"));
        app.drag(stop,{dx:30,dy:0},{steps:5}); app.frames(24,{waitMs:80});
        ({stops:app.snapshot().findAll({role:"slider"}).filter(n=>n.label.startsWith("graph-gradient:stop:")).map(n=>n.label),
          errors:app.snapshot().findAll({role:"text"}).map(n=>n.label).filter(n=>n.includes("failed")||n.includes("invalid"))})
    "#), Duration::from_secs(60));
    assert_eq!(result.error, None, "{}", result.stdout);
    assert!(
        result.result["errors"].as_array().unwrap().is_empty(),
        "{}",
        result.result
    );
    let root = support::config_dir("lighting-gradient-edit");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool =
                sqlx::SqlitePool::connect(&format!("sqlite:{}", root.join("luma.db").display()))
                    .await
                    .unwrap();
            let source: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: serde_json::Value = serde_json::from_str(&source).unwrap();
            let definition = score["definitions"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            let stops = &definition["inputs"]["gradient"]["default"]["value"]["stops"];
            assert!(
                stops[0]["t"].as_f64().unwrap() > 0.01,
                "gradient edit was not persisted: {stops}"
            );
            assert!(stops[0]["color"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c.as_f64() == Some(0.0)));
            pool.close().await;
        });
}

#[test]
fn insert_a_dissolve_pattern_in_the_native_score_editor() {
    let mut harness = Fixture::new("lighting-pattern-insert", 20, vec![])
        .with_graph_score(serde_json::json!({"version":7,"definitions":{},"clips":{}}))
        .with_rig()
        .open(Mode::Headless);
    let result=harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.click(app.snapshot().find({role:"row",label:"Lane 0"}),{button:"right"});
        until("lighting search",s=>s.find({role:"input",label:"Search patterns…"}));
        const field=app.snapshot().find({role:"input",label:"Search patterns…"});
        app.type(field,"dissolve"); app.frames(2);
        const shown=app.snapshot().findAll({role:"row"}).map(n=>n.label);
        app.key("enter");
        until("Beat dissolve clip",s=>s.find({role:"card",label:"Beat dissolve"}));
        app.click(app.snapshot().find({role:"card",label:"Beat dissolve"}));
        const inputs=until("typed clip inputs",s=>s.findAll({role:"input"}).some(n=>n.label.startsWith("Duration"))?s:undefined);
        app.click(app.snapshot().find({role:"button",label:"Make independent"}));
        app.frames(24,{waitMs:80});
        ({shown,fields:inputs.findAll({role:"input"}).map(n=>n.label),errors:app.snapshot().findAll({role:"text"}).map(n=>n.label).filter(n=>n.includes("failed")||n.includes("invalid"))})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}", result.stdout);
    assert!(result.result["shown"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "Beat dissolve"));
    assert!(
        result.result["errors"].as_array().unwrap().is_empty(),
        "{}",
        result.result
    );
    let root = support::config_dir("lighting-pattern-insert");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool =
                sqlx::SqlitePool::connect(&format!("sqlite:{}", root.join("luma.db").display()))
                    .await
                    .unwrap();
            let source: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: serde_json::Value = serde_json::from_str(&source).unwrap();
            assert_eq!(score["clips"].as_object().unwrap().len(), 1);
            assert_eq!(
                score["definitions"].as_object().unwrap().len(),
                2,
                "make independent clones the clip graph"
            );
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM patterns")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(
                count, 0,
                "native insertion never creates a separate pattern record"
            );
            pool.close().await;
        });
}

#[test]
fn edit_a_chase_envelope_per_clip() {
    let mut harness = Fixture::new("lighting-envelope-edit", 20, vec![])
        .with_graph_score(serde_json::json!({"version":7,"definitions":{},"clips":{}}))
        .with_rig()
        .open(Mode::Headless);
    let result=harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.click(app.snapshot().find({role:"row",label:"Lane 0"}),{button:"right"});
        until("lighting search",s=>s.find({role:"input",label:"Search patterns…"}));
        const field=app.snapshot().find({role:"input",label:"Search patterns…"});
        app.type(field,"chase"); app.frames(2);
        const shown=app.snapshot().findAll({role:"row"}).map(n=>n.label);
        app.key("enter");
        until("Chase clip",s=>s.find({role:"card",label:"Beat chase"}));
        app.click(app.snapshot().find({role:"card",label:"Beat chase"}));
        const inputs=until("typed clip inputs",s=>s.findAll({role:"input"}).some(n=>n.label.startsWith("Travel time"))?s:undefined);
        until("envelope",s=>s.find({role:"card",label:"Envelope curve"}));
        app.click(app.snapshot().find({role:"button",label:"Ramp down"}));
        app.frames(4,{waitMs:80});
        app.click(app.snapshot().find({role:"button",label:"Envelope Curve"}));
        app.frames(4,{waitMs:80});
        until("Bézier handle",s=>s.find({role:"slider",label:"Envelope segment 1 handle 1"}));
        const handle=app.snapshot().find({role:"slider",label:"Envelope segment 1 handle 1"});
        app.drag(handle, {dx:30,dy:-20}, {steps:8});
        app.frames(12,{waitMs:80});
        app.click(app.snapshot().find({role:"button",label:"Make independent"}));
        app.frames(24,{waitMs:80});
        app.click(app.snapshot().find({role:"card",label:"Beat chase"}),{count:2});
        until("rendered graph output",s=>s.find({role:"card",label:"Graph workspace"}));
        ({shown,fields:inputs.findAll({role:"input"}).map(n=>n.label),errors:app.snapshot().findAll({role:"text"}).map(n=>n.label).filter(n=>n.includes("failed")||n.includes("invalid"))})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}", result.stdout);
    assert!(result.result["shown"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "Beat chase"));
    assert!(
        result.result["errors"].as_array().unwrap().is_empty(),
        "{}",
        result.result
    );
    let root = support::config_dir("lighting-envelope-edit");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool =
                sqlx::SqlitePool::connect(&format!("sqlite:{}", root.join("luma.db").display()))
                    .await
                    .unwrap();
            let source: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: serde_json::Value = serde_json::from_str(&source).unwrap();
            let clip = score["clips"].as_object().unwrap().values().next().unwrap();
            let args = &clip["inputs"];
            let points = args["shape"]["value"]["points"].as_array().unwrap();
            assert_eq!(points.len(), 2, "bending a segment keeps its two anchors");
            assert_eq!(points[0][0].as_f64(), Some(0.));
            assert_eq!(points[0][1].as_f64(), Some(1.));
            assert_eq!(points[1][0].as_f64(), Some(1.));
            assert_eq!(points[1][1].as_f64(), Some(0.));
            let curve = &args["shape"]["value"]["curves"][0];
            assert_eq!(curve["kind"], "bezier");
            assert!(curve["control1"][0].as_f64().unwrap() > 1. / 3.);
            assert!(curve["control1"][1].as_f64().unwrap() > 2. / 3.);
            let graph = &score["definitions"][clip["graph"].as_str().unwrap()];
            assert_ne!(
                graph["inputs"]["shape"]["default"]["value"]["points"],
                args["shape"]["value"]["points"],
                "clip envelope edits preserve graph defaults"
            );
            pool.close().await;
        });
}

#[test]
fn inspect_builtin_chase_and_customize_its_composition() {
    #[cfg(feature = "pixel")]
    let mode = Mode::Pixel;
    #[cfg(not(feature = "pixel"))]
    let mode = Mode::Headless;
    let mut harness = Fixture::new("lighting-nested-graphs", 20, vec![])
        .with_graph_score(serde_json::json!({"version":7,"definitions":{},"clips":{}}))
        .with_rig()
        .open(mode);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.click(app.snapshot().find({role:"row",label:"Lane 0"}),{button:"right"});
        until("search",s=>s.find({role:"input",label:"Search patterns…"}));
        const field=app.snapshot().find({role:"input",label:"Search patterns…"});
        app.type(field,"chase"); app.frames(2); app.key("enter");
        until("Chase clip",s=>s.find({role:"card",label:"Beat chase"}));
        const clip=app.snapshot().find({role:"card",label:"Beat chase"});
        app.click(clip);
        app.click(app.snapshot().find({role:"card",label:clip.label}),{count:2});
        until("graph preview",s=>s.find({role:"card",label:"Graph workspace"}));
        const root=app.snapshot().findAll({role:"card"}).map(n=>n.label).sort();
        const rootShot=CAPTURE ? app.screenshot() : null;
        app.click(app.snapshot().find({role:"card",label:"Chase"}),{count:2});
        until("chase recipe",s=>s.find({role:"card",label:"Pill"}));
        const chaseShot=CAPTURE ? app.screenshot() : null;
        const kernel=app.snapshot().findAll({role:"card"}).map(n=>n.label).sort();
        app.click(app.snapshot().find({role:"card",label:"Pill"}));
        app.key("backspace"); app.frames(3);
        const afterDelete=app.snapshot().findAll({role:"card"}).map(n=>n.label).sort();
        app.click(app.snapshot().find({role:"button",label:"Graph: Beat chase"}));
        until("back at root",s=>s.find({role:"card",label:"Chase"})&&!s.find({role:"card",label:"Pill"}));
        const rootAfter=app.snapshot().findAll({role:"card"}).map(n=>n.label).sort();
        app.click(app.snapshot().find({role:"card",label:"Chase"}));
        until("customize",s=>s.find({role:"button",label:"Edit a copy"}));
        app.click(app.snapshot().find({role:"button",label:"Edit a copy"}));
        until("editable chase copy",s=>s.find({role:"card",label:"Pill"})&&s.find({role:"button",label:"Add node"}));
        app.key("secondary-z");
        until("undo customization returns to parent",s=>s.find({role:"card",label:"Chase"})&&!s.find({role:"card",label:"Pill"}));
        app.key("secondary-shift-z");
        app.click(app.snapshot().find({role:"card",label:"Chase"}),{count:2});
        until("redo customization is editable",s=>s.find({role:"card",label:"Pill"})&&s.find({role:"button",label:"Add node"}));
        app.click(app.snapshot().find({role:"button",label:"Add node"}));
        until("search copy",s=>s.find({role:"input",label:"Search nodes…"}));
        app.type(app.snapshot().find({role:"input",label:"Search nodes…"}),"Position");
        until("add position",s=>s.find({role:"button",label:"Add Position"}));
        app.click(app.snapshot().find({role:"button",label:"Add Position"}));
        until("edited copy",s=>s.find({role:"card",label:"Position"}));
        app.frames(24,{waitMs:50});
        ({root,rootShot,chaseShot,kernel,afterDelete,rootAfter,customized:true})
    "#.replace("CAPTURE", if cfg!(all(feature = "pixel", target_os = "macos")) { "true" } else { "false" }).as_str()), Duration::from_secs(60));
    assert_eq!(result.error, None, "{}", result.stdout);
    for (key, filename) in [
        ("rootShot", "/tmp/luma-graph-reset-root.png"),
        ("chaseShot", "/tmp/luma-graph-reset-chase.png"),
    ] {
        if let Some(path) = result.result[key]["path"].as_str() {
            std::fs::copy(path, filename).unwrap();
        }
    }
    assert!(!result.result["root"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "Inputs"));
    assert!(result.result["root"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "Chase"));
    assert!(result.result["kernel"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "Pill"));
    assert_eq!(result.result["kernel"], result.result["afterDelete"]);
    assert_eq!(result.result["root"], result.result["rootAfter"]);
    assert_eq!(result.result["customized"], true);
}

#[test]
fn compose_and_wire_outputs_in_the_native_graph_editor() {
    #[cfg(feature = "pixel")]
    let mode = Mode::Pixel;
    #[cfg(not(feature = "pixel"))]
    let mode = Mode::Headless;
    let mut harness = Fixture::new("lighting-compose-outputs", 20, vec![])
        .with_graph_score(serde_json::json!({"version":7,"definitions":{},"clips":{}}))
        .with_rig()
        .window(1600., 1000.)
        .open(mode);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.click(app.snapshot().find({role:"row",label:"Lane 0"}),{button:"right"});
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        app.type(node("input","Search patterns…"),"chase"); app.frames(2); app.key("enter");
        app.click(node("card","Beat chase"));
        app.click(node("card","Beat chase"),{count:2});
        app.drag(node("card","Chase"),{dx:28,dy:18},{steps:5});
        app.frames(8,{waitMs:30});
        app.click(node("card","Chase"));
        app.click(node("button","Edit Input Boundary"));
        node("card","Boundary");
        app.click(node("select","Natural")); app.click(node("button","Wrap"));
        app.click(node("card","Boundary")); app.key("secondary-z");
        node("select","Natural"); app.key("secondary-shift-z"); node("select","Wrap");
        app.click(node("button","Add node"));
        app.type(node("input","Search nodes…"),"Position");
        app.click(node("button","Add Position"));
        until("position",s=>s.find({role:"card",label:"Position"}));
        const place=(label,x,y)=>{
            const b=node("card",label).bounds, w=node("card","Graph workspace").bounds;
            app.drag({x:b.x+b.width/2,y:b.y+8},{dx:w.x+x-b.x,dy:w.y+y-b.y},{steps:8});
            const moved=node("card",label).bounds;
            if(Math.abs(moved.x-w.x-x)>1 || Math.abs(moved.y-w.y-y)>1) throw new Error(JSON.stringify({label,b,w,moved,wanted:[w.x+x,w.y+y],cards:app.snapshot().findAll({role:"card"}).filter(n=>["Chase","Position","Add Lighting"].includes(n.label)).map(n=>({label:n.label,bounds:n.bounds}))}));
        };
        place("Position",500,620);
        place("Chase",500,50);
        place("Apply",760,430);
        const wire=(a,b)=>{app.drag(node("button",a),node("button",b),{steps:8,restale:"match"});app.frames(2);};
        node("button","Edge color.value → output.color");
        wire("write_position_1 output pan","output input pan");
        node("button","Edge write_position_1.pan → output.pan");
        wire("write_position_1 output tilt","output input tilt");
        node("button","Edge write_position_1.tilt → output.tilt");
        if(app.snapshot().find({role:"button",label:"Use as graph output"})) throw new Error("Output required a second terminal selection");
        app.frames(20,{waitMs:50});
        const checkNoStrip = !app.snapshot().find({role:"card",label:"Pattern output preview"});
        if (!checkNoStrip) throw new Error("static output strip still visible");
        ({cards:app.snapshot().findAll({role:"card"}).map(n=>n.label)})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}", result.stdout);
    let root = support::config_dir("lighting-compose-outputs");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool =
                sqlx::SqlitePool::connect(&format!("sqlite:{}", root.join("luma.db").display()))
                    .await
                    .unwrap();
            let source: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: serde_json::Value = serde_json::from_str(&source).unwrap();
            let graph = &score["definitions"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap()["body"]["body"];
            assert!(
                graph["nodes"]["chase"]["position"].is_array(),
                "native layout persists in the graph document"
            );
            assert_eq!(graph["outputs"]["lighting"]["node"], "output");
            assert_eq!(
                graph["nodes"]["output"]["inputs"]["color"]["node"],
                "color"
            );
            assert_eq!(
                graph["nodes"]["output"]["inputs"]["pan"]["node"],
                "write_position_1"
            );
            pool.close().await;
        });
}
