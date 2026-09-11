use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn graph_palettes_keep_empty_defaults_and_overrides_through_editing_and_undo() {
    let name = "graph-empty-palettes";
    let mut harness = Fixture::new(name,20,vec![])
        .with_graph_score(serde_json::json!({
            "version":5,"definitions":{"ribbon":{"name":"Empty colors",
                "inputs":{"palette":{"name":"Palette","description":"","value_type":"gradient","rate":"fixed","default":{"type":"gradient","value":{"stops":[]}}}},
                "outputs":{"lighting":{"value_type":"lighting","rate":"frame"}},
                "body":{"kind":"graph","body":{"input_nodes":{"palette":{"name":"Palette","position":[-250.,0.]}},"nodes":{
                    "mix":{"definition":"mix_palette","position":[50.,0.],"inputs":{"gradient":{"source":"input","input":"palette"},"weights":{"source":"value","value":{"type":"number","value":1.}}}},
                    "output":{"definition":"output","position":[400.,0.],"inputs":{"color":{"source":"connection","node":"mix","output":"color"}}}
                },"outputs":{"lighting":{"source":"connection","node":"output","output":"lighting"}}}}
            }},"clips":{"clip":{"graph":"ribbon","start":0.,"duration":8.,"seed":0,
                "inputs":{"palette":{"type":"gradient","value":{"stops":[{"t":0.4,"color":[1.,0.,0.],"alpha":0.25}]}}}
            }}
        }))
        .with_rig().window(1600.,1000.).open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue");nav.track("Aurora");nav.expand();nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const opacity=()=>{until("Stop opacity",s=>s.findAll({role:"input"}).some(n=>n.label.startsWith("Stop opacity = ")));return app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith("Stop opacity = "));};
        const expect=v=>{const actual=Number(opacity().label.split(" = ")[1]);if(Math.abs(actual-v)>1e-6)throw new Error("opacity "+actual+" != "+v);};
        const empty=()=>{node("text","No colors");if(app.snapshot().findAll({role:"input"}).some(n=>n.label.startsWith("Stop opacity = ")))throw new Error("empty palette still edits a synthetic stop");};
        app.click(node("card","Empty colors"));expect(0.25);
        app.click(node("card","Empty colors"),{count:2});app.click(node("card","Palette"));empty();
        app.click(node("button","Add gradient color"));expect(1);
        app.click(opacity());app.key("secondary-a backspace");app.type(opacity(),"0.4");app.key("enter");expect(0.4);
        app.click(node("button","Remove gradient stop"));empty();
        app.click(node("card","Palette"));app.key("secondary-z");expect(0.4);
        app.key("secondary-shift-z");empty();app.key("secondary-z");expect(0.4);
        app.frames(12,{waitMs:80});app.click(node("button","Aurora"));app.click(node("card","Empty colors"));expect(0.25);
        app.click(node("button","Remove gradient stop"));empty();
        app.frames(12,{waitMs:80});app.click(node("button","Empty colors"));app.click(node("card","Palette"));expect(0.4);
        app.click(node("button","Aurora"));app.click(node("card","Empty colors"));empty();
        app.click(node("button","Add gradient color"));expect(1);
        app.click(node("button","Remove gradient stop"));empty();
        app.frames(12,{waitMs:80});({emptyPalettes:true})
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
            let raw: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: luma_patterns::Score = serde_json::from_str(&raw).unwrap();
            let Some(luma_patterns::Value::Gradient(default)) =
                &score.definitions["ribbon"].inputs["palette"].default
            else {
                panic!()
            };
            let luma_patterns::Value::Gradient(overridden) = &score.clips["clip"].inputs["palette"]
            else {
                panic!()
            };
            assert_eq!(default.stops.len(), 1);
            assert!((default.stops[0].alpha - 0.4).abs() < 1e-6);
            assert!(overridden.stops.is_empty());
            pool.close().await;
        });
}
