//! Exercise the actual chat with a local scripted service, including send geometry.
use gpui::{px, size, AnyView, AppContext};
use gpui_agent::{Config, Harness, Mode};
use luma_lib::agent::model::{scripted::ScriptedModel, ModelEvent, StopReason};
use luma_lib::agent::{AgentService, ThreadScope};
use std::{sync::Arc, time::Duration};

#[allow(dead_code)]
#[path = "../support/session.rs"]
mod session;

#[test]
fn sending_lifts_the_prompt_and_holds_it_while_the_reply_fills_the_room() {
    let dir = std::env::temp_dir().join(format!("luma-send-motion-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap(),
    );
    let (services, scope) = runtime.block_on(async {
        let db = luma_lib::database::local::database::init_app_db_at(&dir)
            .await
            .unwrap();
        session::signed_in(&dir).await;
        let state = luma_lib::database::local::state::init_state_db_at(&dir)
            .await
            .unwrap();
        luma_lib::database::local::auth::bootstrap_headless_admission(&db.0, &state.0)
            .await
            .unwrap();
        let storage = luma_lib::storage::StorageRoot::from_path(dir.clone());
        let workers = Arc::new(
            luma_lib::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("No Python in this fixture".into())),
            ),
        );
        let services =
            luma_lib::dispatch::AppServices::headless(db, state, storage, dir.clone(), workers)
                .with_fixture_principal(Some(session::PRINCIPAL.into()))
                .into_shared();
        let venue = luma_lib::dispatch::dispatch(
            &services,
            "create_venue",
            &serde_json::json!({"name":"Motion fixture", "description":null}),
        )
        .await
        .unwrap();
        (services, ThreadScope::venue(venue["id"].as_str().unwrap()))
    });
    let script = (0..3)
        .map(|ix| {
            let mut events = (0..16)
                .map(|_| ModelEvent::TextDelta("A softly lit stage. ".into()))
                .collect::<Vec<_>>();
            if ix == 1 {
                // Keep thinking after the text stops growing, so the test can
                // observe the indicator revealing without changing geometry.
                events.extend((0..100).map(|_| ModelEvent::TextDelta(String::new())));
            }
            if ix == 2 {
                events = vec![ModelEvent::TextDelta("A softly lit stage.\n\n".repeat(100))];
            }
            events.push(ModelEvent::StepEnded {
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
            });
            events
        })
        .collect();
    let agent = luma_chat::Agent::new(
        AgentService::new(services).with_model(Arc::new(
            ScriptedModel::new(script).with_cadence(Duration::from_millis(35)),
        )),
        runtime.handle().clone(),
    );
    let root: gpui_agent::RootFactory = Arc::new(move |window, cx| -> AnyView {
        luma_app::init(cx);
        cx.new(|cx| luma_chat::AgentChat::new(agent.clone(), Some(scope.clone()), window, cx))
            .into()
    });
    let mut app = Harness::headless(
        Config {
            #[cfg(all(feature = "pixel", target_os = "macos"))]
            mode: Mode::Pixel,
            #[cfg(not(all(feature = "pixel", target_os = "macos")))]
            mode: Mode::Headless,
            window_size: size(px(800.), px(800.)),
            runtime: luma_ui::runtime::Runtime {
                config_dir: Some(dir),
                reduced_motion: false,
                ..Default::default()
            },
            ..Default::default()
        },
        root,
    )
    .unwrap();
    let script = concat!(
        include_str!("../support/until.js"),
        r#"
        until("composer", s => s.find({role:"button", label:"Send"}));
        app.frames(5, {waitMs: 30});
        const send = text => {
            app.type(app.snapshot().find({role:"input", label:"Do anything…"}), text);
            app.frames(2);
            app.click(app.snapshot().find({role:"button", label:"Send"}));
        };
        send("Light the stage");
        until("first reply", s => s.findAll({role:"text"}).some(n => n.label.includes("softly lit")));
        app.frames(70, {waitMs: 20});
        send("Make it warmer");
        app.frames(2, {waitMs: 16});
        const start = app.snapshot().find({role:"card", label:"Sending message"});
        if (app.snapshot().findAll({role:"text"}).some(n => n.label === "Working" || n.label === "Sending")) {
            throw new Error("thinking appeared before the message landed");
        }
        const startShot = capture ? app.screenshot() : null;
        app.frames(8, {waitMs: 16});
        const middle = app.snapshot().find({role:"card", label:"Sending message"});
        const middleShot = capture ? app.screenshot() : null;
        until("message landing", s => !s.find({role:"card", label:"Sending message"}));
        app.frames(4, {waitMs: 20});
        const settled = app.snapshot();
        const prompt = settled.findAll({role:"text"}).find(n => n.label === "Make it warmer");
        const viewport = settled.find({role:"card", label:"Conversation"});
        const settledShot = capture ? app.screenshot() : null;
        const thinking = settled.find({role:"text",label:"Working"});
        if (!thinking) throw new Error("thinking did not appear after landing");
        app.frames(6, {waitMs: 20});
        const stillThinking = app.snapshot().find({role:"text",label:"Working"});
        if (!stillThinking || Math.abs(thinking.bounds.y - stillThinking.bounds.y) > 0.5) {
            throw new Error("thinking moved after the message landed");
        }
        until("second reply completion", s => !s.find({role:"text",label:"Working"}));
        send("Show every cue");
        app.frames(10, {waitMs: 20});
        until("long reply landing", s => !s.find({role:"card", label:"Sending message"}));
        const longReply = app.snapshot();
        ({startShot, middleShot, settledShot, start, middle, prompt, viewport,
          flying: settled.find({role:"card",label:"Sending message"}),
          longFlying: longReply.find({role:"card",label:"Sending message"})})
    "#
    );
    let result = app.exec(
        &format!(
            "globalThis.capture = {};\n{script}",
            cfg!(all(feature = "pixel", target_os = "macos"))
        ),
        Duration::from_secs(60),
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let out = result.result;
    if cfg!(all(feature = "pixel", target_os = "macos")) {
        println!(
            "captures: {} {} {}",
            out["startShot"], out["middleShot"], out["settledShot"]
        );
    }
    assert!(out["start"].is_object(), "{out}");
    assert!(
        out["middle"]["bounds"]["y"].as_f64().unwrap()
            < out["start"]["bounds"]["y"].as_f64().unwrap(),
        "{out}"
    );
    let y = out["prompt"]["bounds"]["y"].as_f64().unwrap();
    let top = out["viewport"]["bounds"]["y"].as_f64().unwrap();
    assert!((y - top - 48.0).abs() < 8.0, "{out}");
    assert!(out["flying"].is_null(), "{out}");
    assert!(out["longFlying"].is_null(), "{out}");
}
