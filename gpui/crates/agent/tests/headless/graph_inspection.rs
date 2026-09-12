use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn graph_inspection_uses_clip_overrides_and_exposes_signal_heads_and_event_times() {
    let name = "graph-inspection";
    let score = serde_json::json!({
        "version":7,"definitions":{"inspect":{"name":"Inspect preview",
            "inputs":{"level":{"name":"Brightness","description":"","value_type":"proportion","rate":"fixed","default":{"type":"proportion","value":0.75}}},
            "outputs":{
                "lighting":{"value_type":"lighting","rate":"frame"},
                "view/level":{"value_type":"proportion","rate":"fixed"},
                "view/heads":{"value_type":{"signal":{}},"rate":"fixed"},
                "view/events":{"value_type":"events","rate":"fixed"}
            },
            "body":{"kind":"graph","body":{"nodes":{
                "heads":{"definition":"fixture_geometry"},
                "tint":{"definition":"core/multiply","inputs":{"a":{"source":"input","input":"level"},"b":{"source":"value","value":{"type":"color","value":[1.0,1.0,1.0]}}}},
                "output":{"definition":"output","inputs":{"color":{"source":"connection","node":"tint","output":"value"}}}
            },"outputs":{
                "lighting":{"source":"connection","node":"output","output":"lighting"},
                "view/level":{"source":"input","input":"level"},
                "view/heads":{"source":"connection","node":"heads","output":"index"},
                "view/events":{"source":"value","value":{"type":"events","value":{"source":"beats","times":[4.,5.,7.]}}}
            }}}
        }},"clips":{"clip":{"graph":"inspect","start":4.,"duration":4.,"seed":0,"inputs":{"level":{"type":"proportion","value":0.25}}}}
    });
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(score.clone())
        .with_rig()
        .window(1600., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue");nav.track("Aurora");nav.expand();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        app.click(node("card","Inspect preview"));app.click(node("card","Inspect preview"),{count:2});
        app.click(node("button","Inspect signals"));node("card","Graph event plot");
        node("text","3 events · global schedule");
        app.click(node("select","events"));app.click(node("button","level"));
        node("card","Graph signal plot");node("text","128 samples · Proportion · 0.250 to 0.250");
        app.click(node("select","level"));app.click(node("button","heads"));
        node("card","Graph signal plot");
        const head=()=>{until("inspection head",s=>s.findAll({role:"text"}).some(n=>n.label.startsWith("Inspect head: ")));return app.snapshot().findAll({role:"text"}).find(n=>n.label.startsWith("Inspect head: ")).label;};
        const first=head();app.click(node("button","Next head"));app.frames(3);
        if(head()===first)throw new Error("head paging did not change the inspected fixture");
        app.click(node("button","Previous head"));app.frames(3);
        if(head()!==first)throw new Error("head paging did not return to the original fixture");
        const slider=node("slider","Preview time");app.drag(slider,{dx:slider.bounds.width*.25,dy:0},{steps:4,restale:"match"});
        node("card","Graph signal plot");
        app.click(node("button","Close inspection"));
        if(app.snapshot().find({role:"card",label:"Graph signal plot"}))throw new Error("closed inspection remained visible");
        app.click(node("button","Inspect signals"));node("card","Graph signal plot");
        ({inspection:true})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
    let dir = support::config_dir(name);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let stored = support::stored_score(&dir).await;
            let expected: luma_patterns::Score = serde_json::from_value(score).unwrap();
            assert_eq!(stored, expected, "inspection changed the authored score");
        });
}
