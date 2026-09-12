use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn graph_spectrogram_uses_overridden_filters_and_keeps_missing_stem_errors_local() {
    let name = "graph-spectrogram";
    let score = serde_json::json!({
        "version":7,"definitions":{"audio":{"name":"Audio inspection",
            "inputs":{"cutoff":{"name":"Cutoff","description":"","value_type":"number","rate":"fixed","default":{"type":"number","value":800.}}},
            "outputs":{
                "lighting":{"value_type":"lighting","rate":"frame"},
                "view/filtered":{"value_type":"audio_source","rate":"fixed"},
                "view/missing":{"value_type":"audio_source","rate":"fixed"},
                "view/raw":{"value_type":"audio_source","rate":"fixed"}
            },
            "body":{"kind":"graph","body":{"nodes":{
                "filter":{"definition":"audio_lowpass","inputs":{"cutoff_hz":{"source":"input","input":"cutoff"}}},
                "output":{"definition":"output","inputs":{"color":{"source":"value","value":{"type":"color","value":[0.25,0.25,0.25]}}}}
            },"outputs":{
                "lighting":{"source":"connection","node":"output","output":"lighting"},
                "view/filtered":{"source":"connection","node":"filter","output":"source"},
                "view/missing":{"source":"value","value":{"type":"audio_source","value":"drums"}},
                "view/raw":{"source":"value","value":{"type":"audio_source","value":"mix"}}
            }}}
        }},"clips":{"clip":{"graph":"audio","start":4.,"duration":4.,"seed":0,"inputs":{"cutoff":{"type":"number","value":200.}}}}
    });
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(score.clone())
        .with_rig_of(0)
        .window(1600., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue");nav.track("Aurora");nav.expand();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const text=prefix=>until(prefix,s=>s.findAll({role:"text"}).some(n=>n.label.startsWith(prefix)));
        app.click(node("card","Audio inspection"));app.click(node("card","Audio inspection"),{count:2});
        app.click(node("button","Inspect signals"));
        const plot=node("card","Graph spectrogram");
        if(plot.bounds.width<100||plot.bounds.height<100)throw new Error("spectrogram has no usable plot area");
        text("mix audio · Lowpass 200 Hz · Mel frequency");
        app.click(node("button","Hide beats"));node("button","Show beats");
        const slider=node("slider","Preview time");app.drag(slider,{dx:slider.bounds.width*.25,dy:0},{steps:5,restale:"match"});
        node("card","Graph spectrogram");
        app.click(node("select","filtered"));app.click(node("button","missing"));
        text("drums stem unavailable;");node("button","Play preview");
        app.click(node("select","missing"));app.click(node("button","raw"));
        text("mix audio · Mel frequency");node("card","Graph spectrogram");
        app.click(node("select","raw"));app.click(node("button","filtered"));
        text("mix audio · Lowpass 200 Hz · Mel frequency");node("button","Show beats");
        app.click(node("button","Close inspection"));app.click(node("button","Inspect signals"));
        node("card","Graph spectrogram");node("button","Show beats");
        ({spectrogram:true})
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
            assert_eq!(
                stored, expected,
                "audio inspection changed the stored score"
            );
        });
}
