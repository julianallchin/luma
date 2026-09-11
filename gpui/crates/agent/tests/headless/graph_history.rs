use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn completed_graph_keeps_each_draft_gesture_and_reopens_an_undone_draft() {
    let mut harness = Fixture::new("graph-draft-history", 20, vec![])
        .with_graph_score(serde_json::json!({
            "version": 4,
            "definitions": {
                "custom": {
                    "name": "Draft history",
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
                                "wash": {
                                    "definition": "wash",
                                    "position": [
                                        750,
                                        320
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
                                        },
                                        "dimmer": {
                                            "source": "connection",
                                            "node": "wash",
                                            "output": "dimmer"
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
    let result=harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const check=(v,m)=>{if(!v)throw new Error(m);};
        const has=(role,label)=>!!app.snapshot().find({role,label});
        const open=()=>{app.click(node("card","Draft history"));app.click(node("card","Draft history"),{count:2});};
        open();
        const add=(query,label,x,y)=>{
            const b=node("card","Graph workspace").bounds;
            app.drag({x:b.x+x,y:b.y+y},{dx:0,dy:0},{button:"right"});
            app.type(node("input","Search nodes…"),query);app.click(node("button",`Add ${label}`));
        };
        add("multiply","Multiply",450,65);
        add("input","Input",40,260);
        const input="$input/input_1 output value";
        const a="Edge $input/input_1.value → core/multiply_1.a";
        const b="Edge $input/input_1.value → core/multiply_1.b";
        app.drag(node("button",input),node("button","core/multiply_1 input a"),{steps:8,restale:"match"});
        node("button",a);
        app.drag(node("button",input),node("button","core/multiply_1 input b"),{steps:8,restale:"match"});
        node("button",b);
        app.key("secondary-z");
        check(!has("button",b) && has("button",a),"completion collapsed earlier draft gestures");
        check(has("card","Multiply") && has("card","Input"),"undo removed the draft's nodes");
        nav.closeTab();open();
        node("button",a);check(!has("button",b),"reopening discarded or completed the draft");
        app.key("secondary-shift-z");node("button",b);
        app.key("secondary-z");check(!has("button",b) && has("button",a),"first undo lost a");
        app.key("secondary-z");check(!has("button",a) && has("card","Input"),"second undo skipped Input creation");
        app.key("secondary-z");check(!has("card","Input") && has("card","Multiply"),"third undo skipped Multiply creation");
        app.key("secondary-z");check(!has("card","Multiply") && has("card","Wash"),"fourth undo lost the original graph");
        app.key("secondary-shift-z");node("card","Multiply");check(!has("card","Input"),"redo skipped a gesture");
        app.key("secondary-shift-z");node("card","Input");check(!has("button",a),"redo connected too early");
        app.key("secondary-shift-z");node("button",a);check(!has("button",b),"redo skipped the final connection");
        app.key("secondary-shift-z");node("button",b);
        app.frames(16,{waitMs:80});
        ({a:has("button",a),b:has("button",b)})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
}
