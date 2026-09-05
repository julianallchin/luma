use super::support::{self, Clip, Fixture};
use gpui_agent::Mode;
use std::time::Duration;
#[test]
fn insert_a_dissolve_pattern_in_the_native_score_editor() {
    let mut harness = Fixture::new(
        "lighting-pattern-insert",
        20,
        vec![Clip::new("wash", "Wash", 0., 2.)],
    )
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
        app.click(app.snapshot().find({role:"button",label:"Save copy to library"}));
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
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let pool=sqlx::SqlitePool::connect(&format!("sqlite:{}",root.join("luma.db").display())).await.unwrap();
        let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM patterns p JOIN track_scores c ON c.pattern_id=p.id AND c.score_id=p.score_id JOIN implementations i ON i.pattern_id=p.id WHERE p.name='Dissolve Flash' AND i.graph_json LIKE '%lighting/dissolve_flash%'").fetch_one(&pool).await.unwrap();
        assert_eq!(count,1,"the native insertion must persist a score-local graph and clip");
        let library_copy:i64=sqlx::query_scalar("SELECT COUNT(*) FROM patterns WHERE name='Dissolve Flash' AND score_id IS NULL").fetch_one(&pool).await.unwrap();
        assert_eq!(library_copy,1,"saving to the library creates an independent global copy");
        pool.close().await;
    });
}

#[test]
fn edit_a_chase_envelope_per_clip() {
    let mut harness = Fixture::new(
        "lighting-envelope-edit",
        20,
        vec![Clip::new("wash", "Wash", 0., 2.)],
    )
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
        app.click(app.snapshot().find({role:"button",label:"Save copy to library"}));
        app.frames(24,{waitMs:80});
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
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let pool=sqlx::SqlitePool::connect(&format!("sqlite:{}",root.join("luma.db").display())).await.unwrap();
        let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM patterns p JOIN track_scores c ON c.pattern_id=p.id AND c.score_id=p.score_id JOIN implementations i ON i.pattern_id=p.id WHERE p.name='Chase' AND i.graph_json LIKE '%lighting/chase%'").fetch_one(&pool).await.unwrap();
        assert_eq!(count,1,"the native insertion must persist a score-local graph and clip");
        let library_copy:i64=sqlx::query_scalar("SELECT COUNT(*) FROM patterns WHERE name='Chase' AND score_id IS NULL").fetch_one(&pool).await.unwrap();
        assert_eq!(library_copy,1,"saving to the library creates an independent global copy");
        let args: String = sqlx::query_scalar("SELECT c.args_json FROM track_scores c JOIN patterns p ON p.id=c.pattern_id WHERE p.name='Chase'").fetch_one(&pool).await.unwrap();
        let args: serde_json::Value = serde_json::from_str(&args).unwrap();
        let points = args["shape"]["points"].as_array().unwrap();
        assert_eq!(points.len(), 3, "a canvas drag adds and moves a custom point");
        assert_eq!(points[0], serde_json::json!([0.,1.]));
        assert_eq!(points[2], serde_json::json!([1.,0.]));
        assert!(points[1][0].as_f64().unwrap() > 0.5);
        assert!(points[1][1].as_f64().unwrap() > 0.5);
        let graph: String = sqlx::query_scalar("SELECT i.graph_json FROM implementations i JOIN patterns p ON p.id=i.pattern_id WHERE p.name='Chase' AND p.score_id IS NOT NULL").fetch_one(&pool).await.unwrap();
        let graph: serde_json::Value = serde_json::from_str(&graph).unwrap();
        assert!(graph["args"].as_array().unwrap().iter().any(|a| a["id"] == "shape" && a["argType"] == "Envelope" && a["defaultValue"]["points"] != args["shape"]["points"]));
        pool.close().await;
    });
}
