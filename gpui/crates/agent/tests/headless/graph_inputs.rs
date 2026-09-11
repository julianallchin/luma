use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn gradient_inputs_preserve_opacity_through_native_edits_undo_and_clip_overrides() {
    use serde_json::json;
    let name = "graph-gradient-opacity";
    let mut harness = Fixture::new(name,20,vec![])
        .with_graph_score(json!({
            "version":6,"definitions":{"ribbon":{"name":"Opacity ribbon",
                "inputs":{"palette":{"name":"Palette","description":"","value_type":"gradient","rate":"fixed","default":{"type":"gradient","value":{"stops":[{"t":0.,"color":[1.,0.,0.],"alpha":0.2},{"t":1.,"color":[0.,0.,1.],"alpha":0.8}]}}}},
                "outputs":{"lighting":{"value_type":"lighting","rate":"frame"}},
                "body":{"kind":"graph","body":{"input_nodes":{"palette":{"name":"Palette","position":[-250.,0.]}},"nodes":{
                    "mix":{"definition":"mix_palette","position":[50.,0.],"inputs":{"gradient":{"source":"input","input":"palette"},"weights":{"source":"value","value":{"type":"number","value":1.}}}},
                    "output":{"definition":"output","position":[400.,0.],"inputs":{"color":{"source":"connection","node":"mix","output":"color"}}}
                },"outputs":{"lighting":{"source":"connection","node":"output","output":"lighting"}}}}
            }},"clips":{"clip":{"graph":"ribbon","start":0.,"duration":8.,"seed":0}}
        }))
        .with_rig().window(1600.,1000.).open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue");nav.track("Aurora");nav.expand();nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const opacity=()=>{until("Stop opacity",s=>s.findAll({role:"input"}).some(n=>n.label.startsWith("Stop opacity = ")));return app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith("Stop opacity = "));};
        const expect=v=>{const actual=Number(opacity().label.split(" = ")[1]);if(Math.abs(actual-v)>1e-6)throw new Error("opacity "+actual+" != "+v);};
        const set=v=>{app.click(opacity());app.key("secondary-a backspace");app.type(opacity(),String(v));app.key("enter");};
        app.click(node("card","Opacity ribbon"));app.click(node("card","Opacity ribbon"),{count:2});
        app.click(node("card","Mix palette"));node("button","Edit Input Palette");
        if(app.snapshot().findAll({role:"input"}).some(n=>n.label.startsWith("Stop opacity = ")))throw new Error("connected gradient remained editable on its consumer");
        app.click(node("button","Edit Input Palette"));expect(0.2);
        app.click(node("slider","graph-gradient:stop:1 = 1"));expect(0.8);
        app.click(node("slider","graph-gradient:stop:0 = 0"));set(0.4);expect(0.4);
        app.click(node("card","Palette"));app.key("secondary-z");expect(0.2);
        app.key("secondary-shift-z");expect(0.4);
        app.frames(12,{waitMs:80});app.click(node("button","Aurora"));app.click(node("card","Opacity ribbon"));
        expect(0.4);set(0.6);expect(0.6);
        app.frames(12,{waitMs:80});app.click(node("button","Opacity ribbon"));app.click(node("card","Palette"));expect(0.4);
        app.frames(12,{waitMs:80});({opacity:true})
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
            assert!((default.stops[0].alpha - 0.4).abs() < 1e-6);
            assert!((overridden.stops[0].alpha - 0.6).abs() < 1e-6);
            assert!((default.stops[1].alpha - 0.8).abs() < 1e-6);
            assert!((overridden.stops[1].alpha - 0.8).abs() < 1e-6);
            pool.close().await;
        });
}

#[test]
fn seed_inputs_preserve_exact_defaults_and_renamed_clip_overrides() {
    use serde_json::json;
    let name = "graph-seed-inputs";
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(json!({
            "version":6,"definitions":{"texture":{"name":"Seeded texture","inputs":{},
                "outputs":{"lighting":{"value_type":"lighting","rate":"frame"}},
                "body":{"kind":"graph","body":{"nodes":{
                    "noise":{"definition":"core/value_noise_1d","position":[300.,0.],"inputs":{
                        "seed":{"source":"value","value":{"type":"seed","value":"18446744073709551614"}},
                        "position":{"source":"value","value":{"type":"number","value":0.25}}
                    }},
                    "tint":{"definition":"core/multiply","position":[480.,0.],"inputs":{"a":{"source":"connection","node":"noise","output":"value"},"b":{"source":"value","value":{"type":"color","value":[1.0,1.0,1.0]}}}},
                    "output":{"definition":"output","position":[650.,0.],"inputs":{"color":{"source":"connection","node":"tint","output":"value"}}}
                },"outputs":{"lighting":{"source":"connection","node":"output","output":"lighting"}}}}
            }},"clips":{"clip":{"graph":"texture","start":0.,"duration":8.,"seed":0}}
        }))
        .with_rig()
        .window(1600., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue");nav.track("Aurora");nav.expand();nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const field=name=>{until(name,s=>s.findAll({role:"input"}).some(n=>n.label.startsWith(name+" = ")));return app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith(name+" = "));};
        const set=(name,value)=>{app.click(field(name));app.key("secondary-a backspace");app.type(field(name),value);app.key("enter");};
        const expect=(name,value)=>{if(!field(name).label.endsWith(" = "+value))throw new Error(name+" lost seed precision: "+field(name).label);};
        app.click(node("card","Seeded texture"));app.click(node("card","Seeded texture"),{count:2});
        const workspace=node("card","Graph workspace").bounds;
        app.drag({x:workspace.x+50,y:workspace.y+workspace.height-80},{dx:0,dy:0});
        app.key("space");app.type(node("input","Search nodes…"),"input");app.click(node("button","Add Input"));
        app.click(node("input","Input name"));app.key("secondary-a backspace");app.type(node("input","Input name"),"Texture seed");app.key("enter");
        app.drag(node("button","$input/input_1 output value"),node("button","noise input seed"),{steps:8,restale:"match"});
        node("button","Edge $input/input_1.value → noise.seed");
        app.click(node("card","Value noise (1D)"));node("button","Edit Input Texture seed");
        if(app.snapshot().findAll({role:"input"}).some(n=>n.label.startsWith("Seed = ")))throw new Error("connected seed remained editable on its consumer");
        app.click(node("button","Edit Input Texture seed"));expect("Value","18446744073709551614");
        set("Value","18446744073709551615");expect("Value","18446744073709551615");
        set("Value","1.5");expect("Value","18446744073709551615");
        app.click(node("card","Texture seed"));app.key("secondary-z");expect("Value","18446744073709551614");
        app.key("secondary-shift-z");expect("Value","18446744073709551615");
        app.frames(12,{waitMs:80});app.click(node("button","Aurora"));app.click(node("card","Seeded texture"));
        expect("Texture seed","18446744073709551615");set("Texture seed","9007199254740993");
        set("Texture seed","18446744073709551616");expect("Texture seed","9007199254740993");
        app.frames(12,{waitMs:80});app.click(node("button","Seeded texture"));app.click(node("card","Texture seed"));
        expect("Value","18446744073709551615");
        app.click(node("input","Input name"));app.key("secondary-a backspace");app.type(node("input","Input name"),"Texture variation");app.key("enter");
        app.frames(12,{waitMs:80});app.click(node("button","Aurora"));app.click(node("card","Seeded texture"));
        expect("Texture variation","9007199254740993");
        ({seed:true})
    "#), Duration::from_secs(60));
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
            let input = &score.definitions["texture"].inputs["input_1"];
            assert_eq!(input.name, "Texture variation");
            assert_eq!(input.value_type, luma_patterns::ValueType::Seed);
            assert_eq!(input.default, Some(luma_patterns::Value::Seed(u64::MAX)));
            assert_eq!(
                score.clips["clip"].inputs["input_1"],
                luma_patterns::Value::Seed(9_007_199_254_740_993)
            );
            pool.close().await;
        });
}

#[test]
fn input_nodes_infer_dropdowns_share_values_and_keep_renamed_clip_overrides() {
    let mut harness = Fixture::new("graph-input-nodes", 20, vec![])
        .with_graph_score(serde_json::json!({
            "version":6,
            "definitions": {
                "custom": {
                    "name": "Input controls",
                    "inputs": {
                        "z_follow": {
                            "name": "Existing control",
                            "description": "",
                            "value_type": "boolean",
                            "rate": "fixed",
                            "default": {
                                "type": "boolean",
                                "value": false
                            }
                        }
                    },
                    "outputs": {
                        "lighting": {
                            "value_type": "lighting",
                            "rate": "frame"
                        }
                    },
                    "body": {
                        "kind": "graph",
                        "body": {
                            "nodes": {
                                "beat_a": {
                                    "definition": "beat_trigger",
                                    "position": [
                                        0,
                                        0
                                    ],
                                    "inputs": {
                                        "grid_aligned": {
                                            "source": "input",
                                            "input": "z_follow"
                                        }
                                    }
                                },
                                "beat_b": {
                                    "definition": "beat_trigger",
                                    "position": [
                                        550,
                                        0
                                    ]
                                },
                                "wash": {
                                    "definition": "wash",
                                    "position": [
                                        550,
                                        400
                                    ]
                                },
                                "initial_output": {
                                    "definition": "output",
                                    "position": [
                                        1000,
                                        400
                                    ],
                                    "inputs": {
                                        "color": {
                                            "source": "connection",
                                            "node": "wash",
                                            "output": "color"
                                        }
                                    }
                                }
                            },
                            "outputs": {
                                "lighting": {
                                    "source": "connection",
                                    "node": "initial_output",
                                    "output": "lighting"
                                }
                            }
                        }
                    }
                }
            },
            "clips": {
                "clip": {
                    "graph": "custom",
                    "start": 0,
                    "duration": 8,
                    "seed": 0
                }
            }
        }))
        .with_rig()
        .window(1600., 1000.)
        .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const check=(value,message)=>{if(!value)throw new Error(message);};
        app.click(node("card","Input controls"));
        app.click(node("card","Input controls"),{count:2});
        const workspace=node("card","Graph workspace").bounds;
        const existing=node("card","Existing control").bounds;
        app.drag({x:workspace.x+60,y:workspace.y+workspace.height-80},{dx:0,dy:0});
        app.key("space"); app.type(node("input","Search nodes…"),"input");
        app.click(node("button","Add Input"));
        node("button","$input/input_1 output value");
        const retained=node("card","Existing control").bounds;
        check(Math.abs(existing.x-retained.x)<1 && Math.abs(existing.y-retained.y)<1,"insertion moved an existing Input");
        const name=node("input","Input name");
        app.click(name); app.key("secondary-a backspace");
        app.type(node("input","Input name"),"Follow beat grid");
        check(!!app.snapshot().find({role:"card",label:"Input"}),"name committed before Enter or blur");
        check(!app.snapshot().find({role:"card",label:"Add node menu"}),"typing a space opened node search");
        app.key("enter"); node("card","Follow beat grid");
        app.drag(node("button","$input/input_1 output value"),node("button","beat_a input grid_aligned"),{steps:8,restale:"match"});
        node("button","Edge $input/input_1.value → beat_a.grid_aligned");
        const beatA=()=>app.snapshot().findAll({role:"card",label:"Beat trigger"})[0];
        app.click(beatA());node("button","Edit Input Follow beat grid");
        check(!app.snapshot().find({role:"select",label:"No"}),"connected dropdown remained editable on its consumer");
        app.click(node("button","Edit Input Follow beat grid"));
        app.click(node("select","No")); app.click(node("button","Yes"));
        app.drag(node("button","beat_b input grid_aligned"),node("button","$input/input_1 output value"),{steps:8,restale:"match"});
        node("button","Edge $input/input_1.value → beat_b.grid_aligned");
        app.click(node("button","Edge $input/input_1.value → beat_a.grid_aligned"));
        app.key("delete");
        node("card","Follow beat grid");
        check(!!app.snapshot().find({role:"button",label:"Edge $input/input_1.value → beat_b.grid_aligned"}),"disconnecting one consumer removed the shared Input");
        app.click(beatA());node("select","No");
        app.key("secondary-z"); node("button","Edge $input/input_1.value → beat_a.grid_aligned");
        node("button","Edit Input Follow beat grid");
        check(!app.snapshot().find({role:"select",label:"Yes"}) && !app.snapshot().find({role:"select",label:"No"}),"undo left a connected dropdown editable");

        app.click(node("button","Aurora"));
        app.click(node("card","Input controls"));
        app.click(node("select","Yes")); app.click(node("button","No"));
        app.frames(12,{waitMs:80});
        app.click(node("button","Input controls"));
        app.click(node("card","Follow beat grid"));
        app.click(node("input","Input name")); app.key("secondary-a backspace");
        app.type(node("input","Input name"),"Stay on grid"); app.key("enter");
        node("card","Stay on grid");
        app.key("secondary-z"); node("card","Follow beat grid");
        app.key("secondary-shift-z"); node("card","Stay on grid");
        app.click(node("card","Stay on grid"));
        node("select","Yes");
        const before=node("card","Stay on grid").bounds;
        app.drag(node("card","Stay on grid"),{dx:30,dy:-40},{steps:8});
        const moved=node("card","Stay on grid").bounds;
        check(Math.abs(moved.x-before.x-30)<1 && Math.abs(moved.y-before.y+40)<1,"Input move lost its layout");
        app.key("secondary-z");
        check(Math.abs(node("card","Stay on grid").bounds.x-before.x)<1,"undo did not restore Input position");
        app.key("secondary-shift-z");
        app.key("delete");
        check(!app.snapshot().find({role:"card",label:"Stay on grid"}),"Delete left Input behind");
        check(!app.snapshot().find({role:"button",label:"Edge $input/input_1.value → beat_b.grid_aligned"}),"deleting Input left its edge");
        app.key("secondary-z");
        node("card","Stay on grid");
        node("button","Edge $input/input_1.value → beat_a.grid_aligned");
        node("button","Edge $input/input_1.value → beat_b.grid_aligned");
        app.frames(16,{waitMs:80});
        ({inputs:app.snapshot().findAll({role:"input"}).map(n=>n.label)})
    "#), Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
    let root = support::config_dir("graph-input-nodes");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool =
                sqlx::SqlitePool::connect(&format!("sqlite:{}", root.join("luma.db").display()))
                    .await
                    .unwrap();
            let text: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: serde_json::Value = serde_json::from_str(&text).unwrap();
            let input = &score["definitions"]["custom"]["inputs"]["input_1"];
            assert_eq!(input["name"], "Stay on grid");
            assert_eq!(input["value_type"], "boolean");
            assert_eq!(
                input["default"],
                serde_json::json!({"type":"boolean","value":true})
            );
            assert_eq!(
                score["clips"]["clip"]["inputs"]["input_1"],
                serde_json::json!({"type":"boolean","value":false})
            );
            for node in ["beat_a", "beat_b"] {
                assert_eq!(
                    score["definitions"]["custom"]["body"]["body"]["nodes"][node]["inputs"]
                        ["grid_aligned"],
                    serde_json::json!({"source":"input","input":"input_1"})
                );
            }
            pool.close().await;
        });
}

#[test]
fn vector_inputs_infer_components_and_edit_defaults_and_clip_overrides_independently() {
    use serde_json::json;
    let name = "graph-vector-inputs";
    let literal = json!({"source":"value","value":{"type":"signal","value":{
        "values":{"v":1,"dim":[1,1,12],"data":[10.,20.,30.,40.,50.,60.,70.,80.,90.,100.,110.,120.]},
        "unit":"degrees","channels":{"components":12},"fixtures":null
    }}});
    let mut harness = Fixture::new(name,20,vec![]).with_graph_score(json!({
        "version":6,"definitions":{"aim":{"name":"Vector aim","inputs":{},
            "outputs":{"lighting":{"value_type":"lighting","rate":"frame"}},
            "body":{"kind":"graph","body":{"nodes":{
                "pan":{"definition":"core/channel","position":[300.,0.],"inputs":{"value":literal,"index":{"source":"value","value":{"type":"number","value":0.}}}},
                "tilt":{"definition":"core/channel","position":[300.,200.],"inputs":{"value":literal,"index":{"source":"value","value":{"type":"number","value":1.}}}},
                "output":{"definition":"output","position":[650.,0.],"inputs":{"pan":{"source":"connection","node":"pan","output":"value"},"tilt":{"source":"connection","node":"tilt","output":"value"}}}
            },"outputs":{"lighting":{"source":"connection","node":"output","output":"lighting"}}}}
        }},"clips":{"clip":{"graph":"aim","start":0.,"duration":8.,"seed":0}}
    })).with_rig().window(1600.,1000.).open(Mode::Headless);
    let result=harness.exec(&support::script(r#"
        nav.venue("Test Venue");nav.track("Aurora");nav.expand();nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const field=name=>{until(name,s=>s.findAll({role:"input"}).some(n=>n.label.startsWith(name+" = ")));return app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith(name+" = "));};
        const set=(name,value)=>{app.click(field(name));app.key("secondary-a backspace");app.type(field(name),String(value));app.key("enter");};
        const expect=(name,value)=>{if(!field(name).label.endsWith(" = "+value))throw new Error(name+" lost another component: "+field(name).label);};
        app.click(node("card","Vector aim"));app.click(node("card","Vector aim"),{count:2});
        const workspace=node("card","Graph workspace").bounds;
        app.drag({x:workspace.x+50,y:workspace.y+workspace.height-80},{dx:0,dy:0});
        app.key("space");app.type(node("input","Search nodes…"),"input");app.click(node("button","Add Input"));
        app.click(node("input","Input name"));app.key("secondary-a backspace");app.type(node("input","Input name"),"Aim");app.key("enter");
        app.drag(node("button","$input/input_1 output value"),node("button","pan input value"),{steps:8,restale:"match"});
        app.drag(node("button","$input/input_1 output value"),node("button","tilt input value"),{steps:8,restale:"match"});
        app.click(node("card","Aim"));expect("Value: 1",10);expect("Value: 2",20);
        app.click(node("button","Aurora"));app.click(node("card","Vector aim"));
        set("Aim (degrees): 1",45);expect("Aim (degrees): 2",20);
        app.click(node("button","Aim (degrees): Next channels"));set("Aim (degrees): 9",99);
        app.click(node("button","Aim (degrees): Previous channels"));expect("Aim (degrees): 1",45);
        app.frames(12,{waitMs:80});app.click(node("button","Vector aim"));app.click(node("card","Aim"));
        expect("Value: 1",10);set("Value: 2",55);expect("Value: 1",10);
        app.click(node("card","Aim"));app.key("secondary-z");expect("Value: 2",20);
        app.key("secondary-shift-z");expect("Value: 2",55);
        app.frames(12,{waitMs:80});app.click(node("button","Aurora"));app.click(node("card","Vector aim"));
        expect("Aim (degrees): 1",45);expect("Aim (degrees): 2",20);
        app.click(node("button","Aim (degrees): Next channels"));expect("Aim (degrees): 9",99);
        ({vector:true})
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
            let input = &score.definitions["aim"].inputs["input_1"];
            assert_eq!(
                input.value_type,
                luma_patterns::ValueType::Signal(luma_patterns::SignalType::new(
                    luma_patterns::Unit::Degrees,
                    luma_patterns::Channels::components(12).unwrap()
                ))
            );
            let luma_patterns::Value::Signal(default) = input.default.as_ref().unwrap() else {
                panic!()
            };
            let luma_patterns::Value::Signal(clip) = &score.clips["clip"].inputs["input_1"] else {
                panic!()
            };
            assert_eq!(default.values()[[0, 0, 0]], 10.);
            assert_eq!(default.values()[[0, 0, 1]], 55.);
            assert_eq!(clip.values()[[0, 0, 0]], 45.);
            assert_eq!(clip.values()[[0, 0, 1]], 20.);
            assert_eq!(clip.values()[[0, 0, 8]], 99.);
            pool.close().await;
        });
}

#[test]
fn rgb_signal_inputs_use_the_color_picker_and_preserve_signal_metadata() {
    use serde_json::json;
    let name = "graph-rgb-input";
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(json!({
            "version":6,"definitions":{"tint":{"name":"RGB input","inputs":{},
                "outputs":{"lighting":{"value_type":"lighting","rate":"frame"}},
                "body":{"kind":"graph","body":{"nodes":{
                    "output":{"definition":"output","position":[300.,0.],"inputs":{
                        "color":{"source":"value","value":{"type":"signal","value":{
                            "values":{"v":1,"dim":[1,1,3],"data":[1.,0.,0.]},
                            "unit":"proportion","channels":"rgb","fixtures":null
                        }}}
                    }}
                },"outputs":{"lighting":{"source":"connection","node":"output","output":"lighting"}}}}
            }},"clips":{"clip":{"graph":"tint","start":0.,"duration":8.,"seed":0}}
        }))
        .with_rig().window(1600.,1000.).open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue");nav.track("Aurora");nav.expand();nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        app.click(node("card","RGB input"));app.click(node("card","RGB input"),{count:2});
        const workspace=node("card","Graph workspace").bounds;
        app.drag({x:workspace.x+50,y:workspace.y+workspace.height-80},{dx:0,dy:0});
        app.key("space");app.type(node("input","Search nodes…"),"input");app.click(node("button","Add Input"));
        app.click(node("input","Input name"));app.key("secondary-a backspace");app.type(node("input","Input name"),"Tint");app.key("enter");
        app.drag(node("button","$input/input_1 output value"),node("button","output input color"),{steps:8,restale:"match"});
        app.click(node("card","Tint"));app.click(node("button","Value swatch"));node("slider","Value:hue");
        if(app.snapshot().find({role:"select",label:"Override"}))throw new Error("RGB input has legacy color modes");
        app.click(node("card","Tint"));
        app.click(node("button","Aurora"));app.click(node("card","RGB input"));
        app.click(node("button","Tint swatch"));
        const hue=node("slider","Tint:hue").bounds;
        app.drag({x:hue.x+1,y:hue.y+hue.height/2},{dx:hue.width/3,dy:0},{steps:8});
        app.click(node("card","RGB input"));app.frames(16,{waitMs:80});
        app.click(node("button","RGB input"));app.click(node("card","Tint"));node("button","Value swatch");
        ({color:true})
    "#), Duration::from_secs(45));
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
            let luma_patterns::Value::Signal(default) = score.definitions["tint"].inputs["input_1"]
                .default
                .as_ref()
                .unwrap()
            else {
                panic!("lost default signal metadata")
            };
            let luma_patterns::Value::Signal(clip) = &score.clips["clip"].inputs["input_1"] else {
                panic!("lost override signal metadata")
            };
            assert_eq!(
                default.values().iter().copied().collect::<Vec<_>>(),
                vec![1., 0., 0.]
            );
            assert_eq!(clip.unit(), luma_patterns::Unit::Proportion);
            assert_eq!(*clip.channels(), luma_patterns::Channels::Rgb);
            assert!(
                clip.values()[[0, 0, 1]] > 0.9 && clip.values()[[0, 0, 0]] < 0.1,
                "color picker did not save green: {clip:?}"
            );
            pool.close().await;
        });
}
