use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn migrated_score_keeps_its_version_labels_and_overrides_after_native_edits() {
    let baseline: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../backend/crates/patterns/tests/fixtures/v2-samples.json"
    ))
    .unwrap();
    let mut score: luma_patterns::Score =
        serde_json::from_value(baseline["score"].clone()).unwrap();
    score.clips.retain(|id, _| id == "sample-chase-1");
    score.definitions.retain(|id, _| id == "sample-chase-1");
    let migrated = luma_patterns::migration::upgrade(&score).unwrap();
    let defaults = migrated.definitions["sample-chase-1"].inputs.clone();
    let original_clip = migrated.clips["sample-chase-1"].clone();
    let name = "graph-migrated-edit";
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(serde_json::to_value(&score).unwrap())
        .with_rig()
        .window(1600., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const check=(v,m)=>{if(!v)throw new Error(m);};
        app.click(node("card","Chase"));
        const field=name=>app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith(name+" = "));
        until("Width",s=>s.findAll({role:"input"}).some(n=>n.label.startsWith("Width (0–1) = ")));
        app.click(field("Width (0–1)")); app.key("secondary-a backspace");
        app.type(field("Width (0–1)"),"0.4"); app.key("enter");
        app.frames(12,{waitMs:80});
        app.click(node("card","Chase"),{count:2});
        node("card","Graph workspace"); node("card","Apply");
        const edge="Edge appearance/mask.value → output.color";
        node("button",edge);
        check(!app.snapshot().find({role:"button",label:"effect output lighting"}),"migration retained a public Lighting socket");
        app.click(node("button",edge)); app.key("delete");
        check(!app.snapshot().find({role:"button",label:edge}),"edge deletion failed after migration");
        app.key("secondary-z"); node("button",edge);
        app.frames(16,{waitMs:80});
        ({restored:true})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
    let dir = support::config_dir(name);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool =
                sqlx::SqlitePool::connect(&format!("sqlite:{}", dir.join("luma.db").display()))
                    .await
                    .unwrap();
            let source: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let saved: serde_json::Value = serde_json::from_str(&source).unwrap();
            assert_eq!(
                saved["version"], luma_patterns::Score::VERSION,
                "native editing downgraded the migrated document"
            );
            let saved: luma_patterns::Score = serde_json::from_value(saved).unwrap();
            saved.validate(&luma_patterns::standard_library()).unwrap();
            assert_eq!(saved.definitions["sample-chase-1"].inputs, defaults);
            let clip = &saved.clips["sample-chase-1"];
            assert_eq!(clip.inputs["color"], original_clip.inputs["color"]);
            assert_eq!(clip.inputs["width"].scalar_value(), Some(0.4));
            assert_eq!(clip.start, original_clip.start);
            assert_eq!(clip.duration, original_clip.duration);
            pool.close().await;
        });
}

#[test]
fn typed_pattern_rows_open_as_editable_score_graphs() {
    let name = "graph-typed-rows";
    let mut harness = Fixture::new(
        name,
        20,
        vec![support::Clip::new("old-chase", "My Chase", 2., 6.)],
    )
    .with_typed_patterns("chase")
    .with_rig()
    .window(1600., 1000.)
    .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        app.click(node("card","My Chase"),{count:2});
        node("card","Graph workspace"); node("card","Apply");
        const edge="Edge appearance/mask.value → output.color";
        app.click(node("button",edge)); app.key("delete");
        if(app.snapshot().find({role:"button",label:edge}))throw new Error("could not edit the converted graph");
        app.key("secondary-z"); node("button",edge);
        app.frames(16,{waitMs:80});
        ({converted:true})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
    let dir = support::config_dir(name);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool =
                sqlx::SqlitePool::connect(&format!("sqlite:{}", dir.join("luma.db").display()))
                    .await
                    .unwrap();
            let source: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: luma_patterns::Score = serde_json::from_str(&source).unwrap();
            assert_eq!(score.version(), luma_patterns::Score::VERSION);
            assert_eq!(score.clips.len(), 1);
            let clip = score.clips.values().next().unwrap();
            assert_eq!((clip.start, clip.duration), (4., 8.));
            assert!(clip.selection_seed.is_some());
            assert_eq!(score.definitions[&clip.graph].name, "My Chase");
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_scores")
                    .fetch_one(&pool)
                    .await
                    .unwrap(),
                0
            );
            assert!(sqlx::query_scalar::<_, String>(
                "SELECT graph_json FROM implementations WHERE pattern_id='old-chase'"
            )
            .fetch_one(&pool)
            .await
            .unwrap()
            .contains("lighting/chase"));
            pool.close().await;
        });
}

#[test]
fn numerical_pattern_rows_open_and_edit_in_the_native_graph() {
    let name = "graph-numerical-rows";
    let mut harness = Fixture::new(
        name,
        20,
        vec![support::Clip::new("pulse", "My Pulse", 2., 6.).lit()],
    )
    .with_rig()
    .window(1600., 1000.)
    .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        app.click(node("card","My Pulse"));
        app.click(node("card","My Pulse"),{count:2});
        node("card","Graph workspace"); node("card","Apply"); node("card","Multiply");
        node("card","intensity"); node("card","tint");
        const edge="Edge red.out → mix.a";
        app.click(node("button",edge)); app.key("delete");
        if(app.snapshot().find({role:"button",label:edge}))throw new Error("could not edit the converted numerical graph");
        app.key("secondary-z"); node("button",edge);
        app.frames(16,{waitMs:80});
        ({converted:true})
    "#), Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
    let dir = support::config_dir(name);
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}",dir.join("luma.db").display())).await.unwrap();
        let raw: String = sqlx::query_scalar("SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL").fetch_one(&pool).await.unwrap();
        let score: luma_patterns::Score = serde_json::from_str(&raw).unwrap();
        score.validate(&luma_patterns::standard_library()).unwrap();
        assert_eq!(score.version(),luma_patterns::Score::VERSION);
        let clip=score.clips.values().next().unwrap();
        let root=&score.definitions[&clip.graph];
        assert_eq!(root.name,"My Pulse");
        assert_eq!(root.inputs["tint"].name,"tint");
        let luma_patterns::Body::Graph(graph)=&root.body else {panic!()};
        assert!(matches!(&graph.nodes["mix"].inputs["a"],luma_patterns::Binding::Connection {node, output} if node=="red" && output=="out"));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM track_scores").fetch_one(&pool).await.unwrap(),0);
        let library: String=sqlx::query_scalar("SELECT graph_json FROM implementations").fetch_one(&pool).await.unwrap();
        assert!(library.contains("sine_wave"),"conversion rewrote the library source");
        pool.close().await;
    });
}

#[test]
fn library_pattern_import_is_score_owned_undoable_and_opens_the_canonical_editor() {
    let name = "graph-library-import";
    let mut harness = Fixture::new(
        name,
        20,
        vec![support::Clip::new("saved-chase", "Saved chase", 8., 12.)],
    )
    .with_typed_patterns("chase")
    .with_rig()
    .window(1600., 1000.)
    .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const count=()=>app.snapshot().findAll({role:"card",label:"Saved chase"}).length;
        node("card","Saved chase");
        nav.patterns(); app.click(node("row","Saved chase"));
        node("card","Insert pattern dialog");
        if(app.snapshot().find({role:"card",label:"Graph workspace"}))throw new Error("browser opened old graph editor");
        app.key("enter");
        until("imported clip",()=>count()===2);
        app.key("secondary-z"); until("undo import",()=>count()===1);
        app.key("secondary-shift-z"); until("redo import",()=>count()===2);
        app.frames(16,{waitMs:80});
        const clips=app.snapshot().findAll({role:"card",label:"Saved chase"});
        const imported=clips.reduce((a,b)=>a.bounds.x<b.bounds.x?a:b);
        app.click(imported,{count:2});
        node("card","Graph workspace"); node("card","Apply");
        const edge="Edge appearance/mask.value → output.color";
        app.click(node("button",edge)); app.key("delete");
        if(app.snapshot().find({role:"button",label:edge}))throw new Error("imported graph is not editable");
        app.key("secondary-z"); node("button",edge);
        app.frames(16,{waitMs:80});
        ({imported:true})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
    let directory = support::config_dir(name);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool = sqlx::SqlitePool::connect(&format!(
                "sqlite:{}",
                directory.join("luma.db").display()
            ))
            .await
            .unwrap();
            let raw: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: luma_patterns::Score = serde_json::from_str(&raw).unwrap();
            score.validate(&luma_patterns::standard_library()).unwrap();
            assert_eq!(score.clips.len(), 2);
            let roots: std::collections::BTreeSet<_> =
                score.clips.values().map(|clip| &clip.graph).collect();
            assert_eq!(
                roots.len(),
                2,
                "import reused the previously authored graph"
            );
            for root in roots {
                assert_eq!(score.definitions[root].name, "Saved chase");
            }
            let library: String = sqlx::query_scalar(
                "SELECT graph_json FROM implementations WHERE pattern_id='saved-chase'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&library).unwrap(),
                serde_json::to_value(luma_lib::node_graph::lighting::pattern("chase").unwrap())
                    .unwrap(),
                "import rewrote the saved library graph",
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM patterns WHERE name='Saved chase'"
                )
                .fetch_one(&pool)
                .await
                .unwrap(),
                1
            );
            pool.close().await;
        });
}
