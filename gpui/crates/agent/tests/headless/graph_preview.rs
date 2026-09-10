use super::support::{self, Fixture};
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn graph_preview_reports_dynamic_errors_stops_audio_and_recovers_on_seek() {
    let name = "graph-preview-runtime-error";
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(serde_json::json!({
            "version": 3,
            "definitions": {
                "custom": {
                    "name": "Runtime preview",
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
                                "clock": {
                                    "definition": "clip_time"
                                },
                                "subtract": {
                                    "definition": "core/subtract",
                                    "inputs": {
                                        "a": {
                                            "source": "value",
                                            "value": {
                                                "type": "number",
                                                "value": 0.5
                                            }
                                        },
                                        "b": {
                                            "source": "connection",
                                            "node": "clock",
                                            "output": "progress"
                                        }
                                    }
                                },
                                "root": {
                                    "definition": "core/square_root",
                                    "inputs": {
                                        "value": {
                                            "source": "connection",
                                            "node": "subtract",
                                            "output": "value"
                                        }
                                    }
                                },
                                "output": {
                                    "definition": "output",
                                    "inputs": {
                                        "dimmer": {
                                            "source": "connection",
                                            "node": "root",
                                            "output": "value"
                                        }
                                    }
                                }
                            },
                            "outputs": {
                                "lighting": {
                                    "source": "connection",
                                    "node": "output",
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
                    "duration": 4,
                    "seed": 0
                }
            }
        }))
        .with_rig()
        .window(1600., 1000.)
        .open(Mode::Headless);
    let dir = support::config_dir(name);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let db = luma_lib::database::local::database::init_app_db_at(&dir)
                .await
                .unwrap();
            luma_lib::venue_graph::ensure_migrated(&db.0, support::VENUE, &dir.join("fixtures"))
                .await
                .unwrap();
            db.0.close().await;
        });
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const check=(v,m)=>{if(!v)throw new Error(m);};
        app.click(node("card","Runtime preview")); app.click(node("card","Runtime preview"),{count:2});
        const seek=fraction=>{
            const slider=node("slider","Preview time");
            app.drag(slider,{dx:slider.bounds.width*(fraction-.5),dy:0},{steps:8,restale:"match"});
        };
        const error="Square root: needs nonnegative values";
        seek(.75); node("text",error);
        seek(.25); app.frames(4,{waitMs:80});
        check(!app.snapshot().find({role:"text",label:error}),"valid seek retained stale evaluation failure");
        app.click(node("button","Play preview")); node("button","Pause preview");
        node("text",error); node("button","Play preview");
        const reading=()=>{
            const t=app.snapshot().findAll({role:"text"}).find(n=>n.label.startsWith("Preview time = "));
            return Number(t.label.slice("Preview time = ".length));
        };
        const stopped=reading(); app.frames(10,{waitMs:80});
        check(Math.abs(reading()-stopped)<.02,"audio continued after evaluation failed");
        check(stopped<1.8,"audio only stopped at the clip end");
        seek(.25); app.frames(4,{waitMs:80});
        check(!app.snapshot().find({role:"text",label:error}),"runtime failure poisoned a later valid seek");
        ({stopped,time:reading()})
    "#), Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
}

#[test]
fn graph_preview_scrubs_real_clip_time_and_owns_its_transport() {
    let name = "graph-clip-preview";
    let expected = serde_json::json!({
        "version": 3,
        "definitions": {
            "custom": {
                "name": "Preview chase",
                "inputs": {
                    "width": {
                        "name": "Width",
                        "description": "",
                        "value_type": "proportion",
                        "rate": "fixed",
                        "default": {
                            "type": "proportion",
                            "value": 0.2
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
                            "chase": {
                                "definition": "chase",
                                "inputs": {
                                    "width": {
                                        "source": "input",
                                        "input": "width"
                                    }
                                }
                            },
                            "initial_output": {
                                "definition": "output",
                                "position": [
                                    1000,
                                    400
                                ],
                                "inputs": {
                                    "dimmer": {
                                        "source": "connection",
                                        "node": "chase",
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
                "start": 4,
                "duration": 4,
                "seed": 0,
                "selection": {
                    "expression": "left_movers"
                },
                "inputs": {
                    "width": {
                        "type": "proportion",
                        "value": 0.5
                    }
                }
            }
        }
    });
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(expected.clone())
        .with_rig()
        .window(1600., 1000.)
        .open(Mode::Headless);
    let dir = support::config_dir(name);
    let before = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let db = luma_lib::database::local::database::init_app_db_at(&dir)
                .await
                .unwrap();
            luma_lib::venue_graph::ensure_migrated(&db.0, support::VENUE, &dir.join("fixtures"))
                .await
                .unwrap();
            let source: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&db.0)
            .await
            .unwrap();
            db.0.close().await;
            source
        });
    let result = harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const check=(v,m)=>{if(!v)throw new Error(m);};
        app.click(node("card","Preview chase")); app.click(node("card","Preview chase"),{count:2});
        node("card","Clip preview transport");
        const slider=node("slider","Preview time");
        check(!app.snapshot().find({role:"card",label:"Pattern output preview"}),"static strip still visible");
        const b=slider.bounds;
        app.drag(slider,{dx:b.width*.25,dy:0},{steps:8,restale:"match"});
        const reading=()=>{
            const t=app.snapshot().findAll({role:"text"}).find(n=>n.label.startsWith("Preview time = "));
            return t ? Number(t.label.slice("Preview time = ".length)) : -1;
        };
        check(reading()>1.2 && reading()<1.8,"scrub did not use the clip's two-second span");
        app.click(node("button","Play preview")); node("button","Pause preview");
        node("button","Play preview");
        check(reading()>1.95,"nonlooping preview did not finish at clip end");
        app.click(node("button","Loop off"));
        app.click(node("button","Play preview")); node("button","Pause preview");
        app.frames(40,{waitMs:80});
        check(!!app.snapshot().find({role:"button",label:"Pause preview"}),"loop stopped at clip end");
        check(reading()>=0 && reading()<2,"loop escaped clip bounds");
        app.click(node("button","Fullscreen visualizer"));
        node("button","Exit fullscreen");
        app.key("space"); node("button","Play preview");
        app.key("space"); node("button","Pause preview");
        check(!app.snapshot().find({role:"input",label:"Search nodes…"}),"fullscreen Space opened the hidden graph search");
        app.key("escape"); node("card","Graph workspace");
        app.click(node("button","Loop on"));
        node("button","Play preview");
        app.click(node("button","Loop off"));
        app.click(node("button","Play preview")); node("button","Pause preview");
        app.click(node("button","Aurora"));
        app.frames(6,{waitMs:80});
        node("text","0:00 / 0:20");
        nav.step("resume the timeline", "button", "Play"); node("button","Pause");
        app.frames(4,{waitMs:80});
        check(app.snapshot().findAll({role:"text"}).some(n=>/^0:0[01] \/ 0:20$/.test(n.label)),"preview replaced the timeline playhead");
        app.click(node("button","Pause")); node("button","Play");
        app.click(node("button","Preview chase"));
        node("button","Play preview");
        const stopped=reading(); app.frames(10,{waitMs:80});
        check(Math.abs(reading()-stopped)<.02,"leaving the graph left preview audio running");
        app.click(node("button","Play preview")); node("button","Pause preview");
        nav.closeTab();
        app.click(node("card","Preview chase")); app.click(node("card","Preview chase"),{count:2});
        node("slider","Preview time"); node("button","Play preview");
        check(reading()===0,"reopened preview retained a removed tab's cursor");
        app.frames(12,{waitMs:80});
        ({time:reading()})
    "#), Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let db = luma_lib::database::local::database::init_app_db_at(&dir)
                .await
                .unwrap();
            let source: String = sqlx::query_scalar(
                "SELECT graph_document_json FROM scores WHERE graph_document_json IS NOT NULL",
            )
            .fetch_one(&db.0)
            .await
            .unwrap();
            assert_eq!(
                source, before,
                "preview controls changed the authored score or clip overrides"
            );
            db.0.close().await;
        });
}
