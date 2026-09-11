use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn graph_output_accepts_composed_signals_and_edits_optional_capabilities() {
    let name = "graph-signal-output";
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(serde_json::json!({
            "version":5,
            "definitions": {
                "custom": {
                    "name": "Signal output",
                    "inputs": {},
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
                                "chase": {
                                    "definition": "beat_chase",
                                    "position": [
                                        0,
                                        0
                                    ]
                                },
                                "tint": {
                                    "definition": "core/multiply",
                                    "position": [
                                        300,
                                        20
                                    ],
                                    "inputs": {
                                        "a": {
                                            "source": "connection",
                                            "node": "chase",
                                            "output": "color"
                                        },
                                        "b": {
                                            "source": "value",
                                            "value": {
                                                "type": "color",
                                                "value": [
                                                    1,
                                                    0,
                                                    0
                                                ]
                                            }
                                        }
                                    }
                                },
                                "wash": {
                                    "definition": "wash",
                                    "position": [
                                        650,
                                        420
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
        const check=(v,m)=>{if(!v)throw new Error(m);};
        const has=label=>app.snapshot().find({role:"button",label});
        app.click(node("card","Signal output")); app.click(node("card","Signal output"),{count:2});
        app.click(node("card","Apply")); app.key("delete");
        const workspace=node("card","Graph workspace").bounds;
        app.drag({x:workspace.x+600,y:workspace.y+45},{dx:0,dy:0},{button:"right"});
        app.type(node("input","Search nodes…"),"apply"); app.click(node("button","Add Apply"));
        node("button","output_1 input color");
        check(!has("output_1 output lighting"),"Output exposed its internal bundle as a socket");
        check(!has("Use as graph output"),"terminal required a second output-selection action");
        app.drag(node("button","tint output value"),node("button","output_1 input pan"),{steps:8,restale:"match"});
        check(!has("Edge tint.value → output_1.pan"),"RGB wire connected to a one-channel pan socket");
        app.drag(node("button","tint output value"),node("button","output_1 input color"),{steps:8,restale:"match"});
        node("button","Edge tint.value → output_1.color");
        app.click(node("card","Apply"));
        const field=prefix=>{
            until(prefix,s=>s.findAll({role:"input"}).some(n=>n.label.startsWith(prefix)));
            return app.snapshot().findAll({role:"input"}).find(n=>n.label.startsWith(prefix));
        };
        node("button","Write Pan (degrees)");
        app.click(field("Pan (degrees)")); app.key("secondary-a backspace");
        app.type(field("Pan (degrees)"),"45"); app.key("enter");
        node("button","Pan (degrees): Clear");
        check(!has("Write Pan (degrees)"),"editing the pan value did not write its capability");
        app.click(node("button","Pan (degrees): Clear")); node("button","Write Pan (degrees)");
        app.key("secondary-z"); node("button","Pan (degrees): Clear");
        check(field("Pan (degrees)").label.includes("45"),"undo lost the angle value");
        const edge="Edge tint.value → output_1.color";
        app.click(node("button",edge)); app.key("delete");
        check(!has(edge),"Delete left the output wire");
        app.key("secondary-z"); node("button",edge);
        app.drag({x:workspace.x+80,y:workspace.y+workspace.height-65},{dx:0,dy:0},{button:"right"});
        app.type(node("input","Search nodes…"),"input"); app.click(node("button","Add Input"));
        app.click(node("input","Input name")); app.key("secondary-a backspace");
        app.type(node("input","Input name"),"Aim"); app.key("enter");
        app.drag(node("button","$input/input_1 output value"),node("button","output_1 input pan"),{steps:8,restale:"match"});
        node("button","Edge $input/input_1.value → output_1.pan");
        check(field("Value").label.includes("45"),"Input did not inherit the angle value");
        app.click(node("button","Aurora")); app.click(node("card","Signal output"));
        app.click(field("Aim (degrees)")); app.key("secondary-a backspace");
        app.type(field("Aim (degrees)"),"-30"); app.key("enter");
        app.frames(12,{waitMs:80});
        app.click(node("button","Signal output")); app.click(node("card","Aim"));
        check(field("Value").label.includes("45"),"clip override changed the graph's default");
        app.frames(16,{waitMs:80});
        ({edge:!!has(edge)})
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
            let text: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: serde_json::Value = serde_json::from_str(&text).unwrap();
            let graph = &score["definitions"]["custom"]["body"]["body"];
            assert_eq!(
                graph["outputs"]["lighting"],
                serde_json::json!({"source":"connection","node":"output_1","output":"lighting"})
            );
            let inputs = &graph["nodes"]["output_1"]["inputs"];
            assert_eq!(
                inputs["color"],
                serde_json::json!({"source":"connection","node":"tint","output":"value"})
            );
            assert_eq!(
                inputs["pan"],
                serde_json::json!({"source":"input","input":"input_1"})
            );
            let input = &score["definitions"]["custom"]["inputs"]["input_1"];
            assert_eq!(
                input["value_type"],
                serde_json::json!({"signal":{"unit":"degrees","channels":"value"}})
            );
            assert_eq!(input["default"]["value"].as_f64(), Some(45.0));
            let override_value = &score["clips"]["clip"]["inputs"]["input_1"];
            assert_eq!(override_value["type"], "degrees");
            assert_eq!(override_value["value"].as_f64(), Some(-30.0));
            assert!(inputs.get("dimmer").is_none());
            pool.close().await;
        });
}
