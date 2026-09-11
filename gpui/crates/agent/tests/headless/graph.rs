//! Canvas gestures operate on the canonical score and its persisted history.
use super::support::{self, Fixture};
use gpui_agent::{Harness, Mode};
use serde_json::{json, Value};
use std::time::Duration;

fn fixture(name: &'static str) -> Harness {
    Fixture::new(name, 20, vec![])
        .with_graph_score(json!({
            "version":7,
            "definitions":{"canvas":{"name":"Canvas test","inputs":{},
                "outputs":{"lighting":{"value_type":"lighting","rate":"frame"}},
                "body":{"kind":"graph","body":{
                    "nodes":{
                        "wash":{"definition":"wash","position":[0,0]},
                        "scale":{"definition":"core/multiply","position":[360,120],"inputs":{
                            "a":{"source":"connection","node":"wash","output":"color"},
                            "b":{"source":"value","value":{"type":"proportion","value":0.5}}
                        }},
                        "output":{"definition":"output","position":[740,240],"inputs":{
                            "color":{"source":"connection","node":"scale","output":"value"}
                        }}
                    },
                    "outputs":{"lighting":{"source":"connection","node":"output","output":"lighting"}}
                }}
            }},
            "clips":{"clip":{"graph":"canvas","start":0,"duration":8,"seed":0}}
        }))
        .with_rig().window(1600.,1000.).open(Mode::Headless)
}

/// The clip preview renders through the stage, so this variant keeps it on.
const OPEN_WITH_STAGE: &str = r#"
    nav.venue("Test Venue"); nav.track("Aurora"); nav.expand();
    const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
    const check=(v,m)=>{if(!v)throw new Error(m);};
    const open=()=>{app.click(node("card","Canvas test"),{count:2});node("card","Graph workspace");node("card","Multiply");};
    open();
"#;

const OPEN: &str = r#"
    nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
    const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
    const check=(v,m)=>{if(!v)throw new Error(m);};
    const open=()=>{app.click(node("card","Canvas test"),{count:2});node("card","Graph workspace");node("card","Multiply");};
    const selected=()=>app.snapshot().findAll({role:"card"}).filter(n=>n.focused).map(n=>n.label).sort();
    open();
"#;

#[test]
fn graph_zoom_controls_preserve_selection_and_restore_the_overview() {
    let mut harness = fixture("graph-visible-zoom");
    let result=harness.exec(&support::script(&format!(r#"
        {OPEN}
        app.click(node("card","Multiply"));
        const before=node("card","Multiply").bounds;
        app.click(node("button","Zoom in"));
        check(node("card","Multiply").bounds.width>before.width*1.2,"Zoom in did not enlarge cards");
        app.click(node("button","Actual size"));
        check(selected().includes("Multiply"),"zoom cleared the node selection");
        app.click(node("button","Fit graph"));
        const fitted=node("card","Multiply").bounds;
        check(Math.abs(fitted.width-before.width)<2,"Fit did not restore the overview");
        check(selected().includes("Multiply"),"Fit cleared the node selection");
    "#)),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
}

#[test]
fn graph_closes_with_its_score_and_reopens_with_saved_edits() {
    let mut harness = fixture("graph-score-lifecycle");
    let result = harness.exec(&support::script(&format!(r#"
        {OPEN}
        app.drag(node("card","Multiply"),{{dx:80,dy:40}},{{steps:8}});
        const moved=node("card","Multiply").bounds;
        app.frames(16,{{waitMs:80}});
        app.click(node("button","Close Aurora"));
        until("score and its graph views closed",s=>!s.find({{role:"button",label:"Canvas test"}})
            && !s.find({{role:"card",label:"Graph workspace"}}));
        nav.track("Aurora");open();
        const restored=node("card","Multiply").bounds;
        // Reopening fits the canvas again, so verify the authored offset by
        // comparing the relative positions rather than window coordinates.
        const wash=node("card","Wash").bounds;
        const output=node("card","Apply").bounds;
        check((restored.x-wash.x)/(output.x-wash.x)>0.5,"closing the score lost its graph edit");
        node("button","Edge scale.value → output.color");
        app.click(node("card","Multiply"));app.key("delete");
        check(!app.snapshot().find({{role:"card",label:"Multiply"}}),"reopened graph does not edit");
        app.key("secondary-z");node("card","Multiply");
        nav.scores("Aurora");app.click(node("button","New score"));
        until("new score",s=>s.find({{role:"text",label:"SCORE #2"}}));
        check(!app.snapshot().find({{role:"button",label:"Canvas test"}}),"switching scores left an orphaned graph tab");
    "#)), Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
}

#[test]
fn graph_drag_is_one_score_edit_and_survives_reopening() {
    let name = "graph-score-drag";
    let mut harness = fixture(name);
    // The preview resolves the clip over the venue's fixtures.
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
    let result=harness.exec(&support::script(&format!(r#"
        {OPEN_WITH_STAGE}
        node("slider","Preview time");
        app.click(node("card","Multiply"));
        const workspace=node("card","Graph workspace").bounds;
        const stable=()=>{{
            const now=app.snapshot().find({{role:"card",label:"Graph workspace"}}).bounds;
            check(now.y===workspace.y && now.height===workspace.height,"layout edit shifted the toolbar");
            check(!!app.snapshot().find({{role:"slider",label:"Preview time"}}),"layout edit discarded the preview");
        }};
        const before=node("card","Multiply").bounds;
        app.drag(node("card","Multiply"),{{dx:80,dy:40}},{{steps:8}});
        stable();
        let moved=node("card","Multiply").bounds;
        check(Math.abs(moved.x-before.x-80)<2 && Math.abs(moved.y-before.y-40)<2,"drag did not follow pointer");
        app.key("secondary-z");
        stable();
        let undone=node("card","Multiply").bounds;
        check(Math.abs(undone.x-before.x)<2 && Math.abs(undone.y-before.y)<2,"drag took more than one undo");
        app.key("secondary-shift-z");
        stable();
        moved=node("card","Multiply").bounds;
        check(Math.abs(moved.x-before.x-80)<2,"redo lost movement");
        app.frames(16,{{waitMs:80}});nav.closeTab();open();
        node("card","Apply");({{moved:true}})
    "#)),Duration::from_secs(60));
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
            let score: Value = serde_json::from_str(&raw).unwrap();
            let nodes = &score["definitions"]["canvas"]["body"]["body"]["nodes"];
            assert!(nodes["scale"]["position"][0].as_f64().unwrap() > 360.);
            assert!(nodes["scale"]["position"][1].as_f64().unwrap() > 120.);
            assert_eq!(nodes["wash"]["position"], json!([0, 0]));
            assert_eq!(nodes["output"]["position"], json!([740, 240]));
            pool.close().await;
        });
}

#[test]
fn graph_ports_selection_and_marquee_keep_parameters_in_the_inspector() {
    let mut harness = fixture("graph-score-selection");
    let result=harness.exec(&support::script(&format!(r#"
        {OPEN}
        app.click(node("button","scale input a"));
        check(selected().includes("Multiply"),"port press did not select its card");
        app.click(node("card","Wash"));app.click(node("card","Multiply"),{{modifiers:["shift"]}});
        check(selected().includes("Wash") && selected().includes("Multiply"),"shift selection failed");
        check(!app.snapshot().nodes.some(n=>n.label.includes(" param ")),"old card parameters remain");
        node("card","Node inspector");
        const cards=["Wash","Multiply","Apply"].map(label=>node("card",label).bounds);
        const left=Math.min(...cards.map(b=>b.x)),top=Math.min(...cards.map(b=>b.y));
        const right=Math.max(...cards.map(b=>b.x+b.width)),bottom=Math.max(...cards.map(b=>b.y+b.height));
        app.drag({{x:left-6,y:top-6}},{{dx:right-left+12,dy:bottom-top+12}},{{modifiers:["shift"]}});
        check(["Wash","Multiply","Apply"].every(label=>selected().includes(label)),"marquee missed nodes");
        app.key("delete");
        check(!app.snapshot().find({{role:"card",label:"Multiply"}}),"multi-delete failed");
        app.key("secondary-z");node("card","Multiply");node("card","Wash");node("card","Apply");
        app.key("secondary-shift-z");check(!app.snapshot().find({{role:"card",label:"Multiply"}}),"redo failed");
        app.key("secondary-z");node("button","Edge scale.value → output.color");
        ({{restored:true}})
    "#)),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
}

#[test]
fn graph_missing_implementation_reports_an_error_and_preserves_saved_rows() {
    let name = "graph-missing-source";
    let mut harness = Fixture::new(
        name,
        20,
        vec![support::Clip::new("missing", "Missing pattern", 2., 6.)],
    )
    .with_rig()
    .window(1600., 1000.)
    .open(Mode::Headless);
    let result=harness.exec(&support::script(r#"
        nav.venue("Test Venue");nav.track("Aurora");nav.expand();nav.stageOff();
        until("clip",s=>s.find({role:"card",label:"Missing pattern"}));
        app.click(app.snapshot().find({role:"card",label:"Missing pattern"}),{count:2});
        until("missing source error",s=>s.nodes.some(n=>n.label.includes("missing or unsupported pattern implementations")));
        if(app.snapshot().find({role:"card",label:"Graph workspace"}))throw new Error("opened an obsolete editor");
        ({preserved:true})
    "#),Duration::from_secs(45));
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
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_scores")
                    .fetch_one(&pool)
                    .await
                    .unwrap(),
                1
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM scores WHERE graph_document_json IS NOT NULL"
                )
                .fetch_one(&pool)
                .await
                .unwrap(),
                0
            );
            pool.close().await;
        });
}

/// A document that places nothing is laid out along its wires: columns run
/// left to right, each Input sits beside the card that reads it, and a graph
/// without an Apply node ends at a card of its named outputs.
#[test]
fn graph_auto_layout_follows_the_wires_and_shows_named_outputs() {
    let name = "graph-auto-layout";
    let mut harness = Fixture::new(name, 20, vec![])
        .with_graph_score(json!({
            "version":7,
            "definitions":{"loose":{"name":"Loose graph",
                "inputs":{
                    "tint":{"name":"Tint","description":"","value_type":{"signal":{"unit":"proportion","channels":"rgb"}},"rate":"frame","default":{"type":"color","value":[1.0,0.5,0.25]}},
                    "amount":{"name":"Amount","description":"","value_type":"proportion","rate":"frame","default":{"type":"proportion","value":0.5}}
                },
                "outputs":{"lighting":{"value_type":"lighting","rate":"frame"}},
                "body":{"kind":"graph","body":{
                    "nodes":{
                        "wash":{"definition":"wash","inputs":{"color":{"source":"input","input":"tint"}}},
                        "scale":{"definition":"core/multiply","inputs":{
                            "a":{"source":"connection","node":"wash","output":"color"},
                            "b":{"source":"input","input":"amount"}
                        }},
                        "output":{"definition":"output","inputs":{
                            "color":{"source":"connection","node":"scale","output":"value"}
                        }}
                    },
                    "outputs":{"lighting":{"source":"connection","node":"output","output":"lighting"}}
                }}
            }},
            "clips":{"clip":{"graph":"loose","start":0,"duration":8,"seed":0}}
        }))
        .with_rig().window(1600.,1000.).open(Mode::Headless);
    let result=harness.exec(&support::script(r#"
        nav.venue("Test Venue"); nav.track("Aurora"); nav.expand(); nav.stageOff();
        const node=(role,label)=>{until(label,s=>s.find({role,label}));return app.snapshot().find({role,label});};
        const check=(v,m)=>{if(!v)throw new Error(m);};
        app.click(node("card","Loose graph"),{count:2});
        node("card","Graph workspace");
        const box=label=>node("card",label).bounds;
        const [tint,amount,wash,scale,apply]=["Tint","Amount","Wash","Multiply","Apply"].map(box);
        const leftOf=(a,b,m)=>check(a.x+a.width<b.x,m);
        leftOf(tint,wash,"Tint is not left of Wash");
        leftOf(wash,scale,"Wash is not left of Multiply");
        leftOf(amount,scale,"Amount is not left of Multiply");
        leftOf(scale,apply,"Multiply is not left of Apply");
        check(amount.x>tint.x+tint.width/2,"Amount did not move beside the card that reads it");
        const mid=b=>b.y+b.height/2;
        check(mid(tint)>wash.y&&mid(tint)<wash.y+wash.height,"Tint is not level with Wash");
        check(mid(amount)>scale.y&&mid(amount)<scale.y+scale.height,"Amount is not level with Multiply");
        const cards=[tint,amount,wash,scale,apply];
        for(let i=0;i<cards.length;i++)for(let j=i+1;j<cards.length;j++){
            const a=cards[i],b=cards[j];
            check(a.x+a.width<=b.x||b.x+b.width<=a.x||a.y+a.height<=b.y||b.y+b.height<=a.y,"cards overlap");
        }
        app.click(node("card","Wash"),{count:2});
        node("card","Outputs"); node("card","Color");
        node("button","Edge $input/color.value → $outputs.color");
        const color=box("Color"), outputs=box("Outputs");
        check(color.x+color.width<outputs.x,"the Input is not left of the outputs card");
        ({laid_out:true})
    "#),Duration::from_secs(60));
    assert_eq!(result.error, None, "{}\n{}", result.stdout, result.result);
}
