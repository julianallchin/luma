use super::{self as support, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

pub fn exercise(mode: Mode, name: &'static str) {
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(serde_json::json!({
            "version":6,
            "definitions": {
                "playground": {
                    "name": "Wire playground",
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
                                "gradient": {
                                    "definition": "sample_gradient"
                                },
                                "wash": {
                                    "definition": "wash"
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
                    "graph": "playground",
                    "start": 0,
                    "duration": 8,
                    "seed": 0
                }
            }
        }))
        .with_rig()
        .window(1600., 1000.)
        .open(mode);
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const check=(condition,message)=>{if(!condition)throw new Error(message);};
        const has=label=>app.snapshot().find({role:"button",label});
        app.click(node("card","Wire playground"));
        app.click(node("card","Wire playground"),{count:2});
        node("button","gradient output color");
        const workspace=node("card","Graph workspace").bounds;
        const at={x:workspace.x+40,y:workspace.y+35};
        app.drag(at,{dx:0,dy:0},{button:"right"});
        const popup=node("card","Add node menu").bounds;
        check(Math.abs(popup.x-at.x)<12 && Math.abs(popup.y-at.y)<12,"search did not open at right-click: "+JSON.stringify({at,popup}));
        // Physical typing must work immediately; app.type clicks the field first.
        app.key("z z z z");
        check(!has("Add Chase"), "right-click search did not receive immediate typing");
        app.key("enter");
        check(!!app.snapshot().find({role:"card",label:"Add node menu"}),"empty search unexpectedly inserted a node");
        app.key("escape");
        check(!app.snapshot().find({role:"card",label:"Add node menu"}),"Escape left search open");

        app.drag(at,{dx:0,dy:0},{button:"right"});
        app.type(node("input","Search nodes…"),"sample gradient");
        check(!!has("Add Sample gradient") && !has("Add Sample gradient per head"),"gradient search exposes scalar/field duplicates");
        app.key("escape");app.drag(at,{dx:0,dy:0},{button:"right"});
        app.type(node("input","Search nodes…"),"envelope");
        check(!!has("Add Evaluate Envelope") && !has("Add Sample envelope per head"),"envelope search exposes scalar/field duplicates: "+JSON.stringify(app.snapshot().findAll({role:"button"}).map(n=>n.label)));
        app.key("escape");

        const insert={x:workspace.x+150,y:workspace.y+workspace.height-120};
        app.drag(insert,{dx:0,dy:0}); app.key("space");
        node("card","Add node menu");
        app.key("b e a t space t r i g g e r");
        app.key("down up enter");
        node("button","beat_trigger_1 output trigger");
        const inserted=node("card","Beat trigger").bounds;
        check(Math.abs(inserted.x-insert.x)<2 && Math.abs(inserted.y-insert.y)<2,"node did not stay at invocation point: "+JSON.stringify({insert,inserted}));
        app.key("secondary-z");
        check(!has("beat_trigger_1 output trigger"),"one undo did not remove inserted node");
        app.key("secondary-shift-z");
        node("button","beat_trigger_1 output trigger");
        const redone=node("card","Beat trigger").bounds;
        check(Math.abs(redone.x-inserted.x)<1 && Math.abs(redone.y-inserted.y)<1,"redo lost insertion position");

        const edge="Edge gradient.color → wash.color";
        const before=node("button","wash input color").bounds;
        app.drag(node("button","gradient output color"),node("button","wash input color"),{steps:8,restale:"match"});
        node("button",edge);
        const after=node("button","wash input color").bounds;
        check(Math.abs(before.x-after.x)<1 && Math.abs(before.y-after.y)<1,"wiring reframed the graph");
        app.drag(node("button","beat_trigger_1 output trigger"),node("button","wash input color"),{steps:8,restale:"match"});
        node("text","expected Signal (0–1, RGB), got Events");
        check(!!has(edge),"rejected wire removed existing connection");
        app.key("secondary-z");
        check(!has(edge),"rejected connection added an undo step");
        app.key("secondary-shift-z");
        node("button",edge);
        app.click(node("button",edge));
        check(node("button",edge).focused,"edge did not become selected");
        app.key("delete");
        check(!has(edge),"Delete did not remove the selected edge");
        app.key("secondary-z"); node("button",edge);
        app.key("secondary-shift-z");
        check(!has(edge),"redo did not remove edge");
        app.drag(node("button","wash input color"),node("button","gradient output color"),{steps:8,restale:"match"});
        node("button",edge);
        const selectHeader=(label,shift)=>{
            const b=node("card",label).bounds;
            app.drag({x:b.x+b.width/2,y:b.y+8},{dx:0,dy:0},{modifiers:shift?["shift"]:[]});
        };
        selectHeader("Sample gradient",false); selectHeader("Wash",true);
        app.key("delete");
        check(!has("gradient output color") && !has("wash input color"),"multi-selection delete left nodes");
        app.key("secondary-z");
        node("button","gradient output color"); node("button","wash input color");
        app.key("secondary-shift-z");
        check(!has("gradient output color") && !has("wash input color"),"redo lost the incomplete draft");
        app.key("secondary-z"); node("button",edge);
        app.key("secondary-z");
        check(!has(edge),"undo did not cross back into valid history");
        app.key("secondary-shift-z"); node("button",edge);
        app.key("secondary-shift-z");
        check(!has("gradient output color") && !has("wash input color"),"draft redo was lost while stepping through valid history");
        app.key("secondary-z"); node("button",edge);
        app.frames(16,{waitMs:80});
        ({inserted,redone,edge:!!has(edge)})
    "#), Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
    #[cfg(feature = "pixel")]
    if matches!(mode, Mode::Pixel) {
        let capture = harness.exec(
            r#"
            const graph=app.screenshot();
            const b=app.snapshot().find({role:"card",label:"Graph workspace"}).bounds;
            app.drag({x:b.x+b.width/2,y:b.y+60},{dx:0,dy:0},{button:"right"});
            app.frames(3);
            const search=app.screenshot();
            ({graph,search})
        "#,
            Duration::from_secs(30),
        );
        assert_eq!(capture.error, None, "{}", capture.stdout);
        for kind in ["graph", "search"] {
            let (path, _) = support::image::keep_in("graph-editor", &capture.result[kind], kind);
            eprintln!("{kind}: {}", path.display());
        }
    }
    let root = support::config_dir(name);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let pool =
                sqlx::SqlitePool::connect(&format!("sqlite:{}", root.join("luma.db").display()))
                    .await
                    .unwrap();
            let document: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let score: serde_json::Value = serde_json::from_str(&document).unwrap();
            let nodes = &score["definitions"]["playground"]["body"]["body"]["nodes"];
            assert_eq!(
                nodes["wash"]["inputs"]["color"],
                serde_json::json!({"source":"connection","node":"gradient","output":"color"})
            );
            assert!(nodes["beat_trigger_1"]["position"].is_array());
            assert_eq!(nodes.as_object().unwrap().len(), 4);
            pool.close().await;
        });
}
