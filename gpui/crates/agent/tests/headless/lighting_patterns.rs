use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;
#[test]
fn insert_a_dissolve_pattern_in_the_native_score_editor() {
    let mut harness = Fixture::new("lighting-pattern-insert", 20, vec![])
        .with_graph_score(serde_json::json!({"version":2,"definitions":{},"clips":{}}))
        .with_rig()
        .open(Mode::Headless);
    let result=harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.click(app.snapshot().find({role:"row",label:"Lane 0"}),{button:"right"});
        until("lighting search",s=>s.find({role:"input",label:"Search lighting nodes and Patterns…"}));
        const field=app.snapshot().find({role:"input",label:"Search lighting nodes and Patterns…"});
        app.type(field,"dissolve"); app.frames(2);
        const shown=app.snapshot().findAll({role:"row"}).map(n=>n.label);
        app.key("enter");
        until("Dissolve Flash clip",s=>s.find({role:"card",label:"Dissolve Flash"}));
        app.click(app.snapshot().find({role:"card",label:"Dissolve Flash"}));
        const inputs=until("typed clip inputs",s=>s.findAll({role:"input"}).some(n=>n.label.startsWith("Travel time (beats)"))?s:undefined);
        app.click(app.snapshot().find({role:"button",label:"Make independent"}));
        app.frames(24,{waitMs:80});
        ({shown,fields:inputs.findAll({role:"input"}).map(n=>n.label),errors:app.snapshot().findAll({role:"text"}).map(n=>n.label).filter(n=>n.includes("failed")||n.includes("invalid"))})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}", result.stdout);
    assert!(result.result["shown"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "Dissolve Flash"));
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
        .with_graph_score(serde_json::json!({"version":2,"definitions":{},"clips":{}}))
        .with_rig()
        .open(Mode::Headless);
    let result=harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.click(app.snapshot().find({role:"row",label:"Lane 0"}),{button:"right"});
        until("lighting search",s=>s.find({role:"input",label:"Search lighting nodes and Patterns…"}));
        const field=app.snapshot().find({role:"input",label:"Search lighting nodes and Patterns…"});
        app.type(field,"chase"); app.frames(2);
        const shown=app.snapshot().findAll({role:"row"}).map(n=>n.label);
        app.key("enter");
        until("Chase clip",s=>s.find({role:"card",label:"Chase"}));
        app.click(app.snapshot().find({role:"card",label:"Chase"}));
        const inputs=until("typed clip inputs",s=>s.findAll({role:"input"}).some(n=>n.label.startsWith("Travel time (beats)"))?s:undefined);
        until("envelope",s=>s.find({role:"card",label:"Envelope curve"}));
        app.click(app.snapshot().find({role:"button",label:"Ramp down"}));
        app.frames(4,{waitMs:80});
        app.drag(app.snapshot().find({role:"card",label:"Envelope curve"}), {dx:30,dy:-20}, {steps:8});
        app.frames(12,{waitMs:80});
        app.click(app.snapshot().find({role:"button",label:"Make independent"}));
        app.frames(24,{waitMs:80});
        app.click(app.snapshot().find({role:"card",label:"Chase"}),{count:2});
        until("rendered graph output",s=>s.find({role:"card",label:"Pattern output preview"}));
        ({shown,fields:inputs.findAll({role:"input"}).map(n=>n.label),errors:app.snapshot().findAll({role:"text"}).map(n=>n.label).filter(n=>n.includes("failed")||n.includes("invalid"))})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}", result.stdout);
    assert!(result.result["shown"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "Chase"));
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
            assert_eq!(
                points.len(),
                3,
                "a canvas drag adds and moves a custom point"
            );
            assert_eq!(points[0][0].as_f64(), Some(0.));
            assert_eq!(points[0][1].as_f64(), Some(1.));
            assert_eq!(points[2][0].as_f64(), Some(1.));
            assert_eq!(points[2][1].as_f64(), Some(0.));
            assert!(points[1][0].as_f64().unwrap() > 0.5);
            assert!(points[1][1].as_f64().unwrap() > 0.5);
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
fn inspect_nested_builtin_graphs_without_an_inputs_box_or_mutating_them() {
    #[cfg(feature = "pixel")]
    let mode = Mode::Pixel;
    #[cfg(not(feature = "pixel"))]
    let mode = Mode::Headless;
    let mut harness = Fixture::new("lighting-nested-graphs", 20, vec![])
        .with_graph_score(serde_json::json!({"version":2,"definitions":{},"clips":{}}))
        .with_rig()
        .open(mode);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.click(app.snapshot().find({role:"row",label:"Lane 0"}),{button:"right"});
        until("search",s=>s.find({role:"input",label:"Search lighting nodes and Patterns…"}));
        const field=app.snapshot().find({role:"input",label:"Search lighting nodes and Patterns…"});
        app.type(field,"chase"); app.frames(2); app.key("enter");
        until("Chase clip",s=>s.find({role:"card",label:"Chase"}));
        const clip=app.snapshot().find({role:"card",label:"Chase"});
        app.click(clip,{count:2});
        until("graph preview",s=>s.find({role:"card",label:"Pattern output preview"}));
        const root=app.snapshot().findAll({role:"card"}).map(n=>n.label);
        const rootShot=CAPTURE ? app.screenshot() : null;
        app.click(app.snapshot().find({role:"card",label:"Chase"}),{count:2});
        until("chase recipe",s=>s.find({role:"card",label:"Chase Mask"}));
        const chaseShot=CAPTURE ? app.screenshot() : null;
        app.click(app.snapshot().find({role:"card",label:"Chase Mask"}),{count:2});
        until("mask recipe",s=>s.find({role:"button",label:"Graph: Chase Mask"}));
        app.click(app.snapshot().find({role:"card",label:"Pill"}),{count:2});
        until("pill recipe",s=>s.find({role:"button",label:"Graph: Pill"}));
        const pill=app.snapshot().findAll({role:"card"}).map(n=>n.label);
        app.click(app.snapshot().find({role:"card",label:"Coordinate offset"}));
        app.key("backspace"); app.frames(3);
        const afterDelete=app.snapshot().findAll({role:"card"}).map(n=>n.label);
        app.click(app.snapshot().find({role:"button",label:"Graph: Chase"}));
        until("back at root",s=>s.find({role:"card",label:"Chase"}));
        ({root,rootShot,chaseShot,pill,afterDelete,rootAfter:app.snapshot().findAll({role:"card"}).map(n=>n.label)})
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
    assert!(result.result["pill"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "Coordinate offset"));
    assert_eq!(result.result["pill"], result.result["afterDelete"]);
    assert_eq!(result.result["root"], result.result["rootAfter"]);
}

#[test]
fn compose_and_wire_outputs_in_the_native_graph_editor() {
    #[cfg(feature = "pixel")]
    let mode = Mode::Pixel;
    #[cfg(not(feature = "pixel"))]
    let mode = Mode::Headless;
    let mut harness = Fixture::new("lighting-compose-outputs", 20, vec![])
        .with_graph_score(serde_json::json!({"version":2,"definitions":{},"clips":{}}))
        .with_rig()
        .open(mode);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        until("waveform",s=>s.find({role:"card",label:"Waveform"}));
        app.click(app.snapshot().find({role:"row",label:"Lane 0"}),{button:"right"});
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        app.type(node("input","Search lighting nodes and Patterns…"),"chase"); app.frames(2); app.key("enter");
        app.click(node("card","Chase"),{count:2});
        app.drag(node("card","Chase"),{dx:28,dy:18},{steps:5});
        app.frames(8,{waitMs:30});
        app.click(node("card","Chase"));
        app.click(node("button","Boundary: On clip · make local"));
        until("local boundary",s=>s.find({role:"button",label:"Boundary: Expose on clip"}));
        app.click(node("button","Boundary: Expose on clip"));
        until("exposed boundary",s=>s.find({role:"button",label:"Boundary: On clip · make local"}));
        app.key("secondary-z");
        until("undo exposure",s=>s.find({role:"button",label:"Boundary: Expose on clip"}));
        app.key("secondary-shift-z");
        until("redo exposure",s=>s.find({role:"button",label:"Boundary: On clip · make local"}));
        app.click(node("button","Add node"));
        app.type(node("input","Search nodes…"),"Position output");
        app.click(node("button","Add Position output"));
        until("position",s=>s.find({role:"card",label:"Position output"}));
        app.click(node("button","Add node"));
        app.type(node("input","Search nodes…"),"Add Lighting");
        app.click(node("button","Add Add Lighting"));
        until("draft",s=>s.findAll({role:"text"}).some(n=>n.label.includes("Draft")));
        const wire=(a,b)=>{app.click(node("button",a));app.click(node("button",b));app.frames(2);};
        wire("effect output lighting","add_lighting_1 input a");
        wire("write_position_1 output lighting","add_lighting_1 input b");
        until("valid graph",s=>!s.findAll({role:"text"}).some(n=>n.label.includes("Draft")));
        app.click(node("button","add_lighting_1 output lighting"));
        app.click(node("button","Use as graph output"));
        app.frames(20,{waitMs:50});
        until("preview",s=>s.find({role:"card",label:"Pattern output preview"}));
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
                graph["nodes"]["effect"]["position"].is_array(),
                "native layout persists in the graph document"
            );
            assert_eq!(graph["outputs"]["lighting"]["node"], "add_lighting_1");
            assert_eq!(
                graph["nodes"]["add_lighting_1"]["inputs"]["a"]["node"],
                "effect"
            );
            assert_eq!(
                graph["nodes"]["add_lighting_1"]["inputs"]["b"]["node"],
                "write_position_1"
            );
            pool.close().await;
        });
}
