//! Cached views must stay inspectable and interactive without rendering again.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{canvas, div, px, App, Context, Entity, StyleRefinement, Window};
use gpui_agent::{Config, Harness};
use luma_ui::node::{agent_paint_node, cached_view, Instrument, Role};

struct Child {
    count: usize,
    renders: Arc<AtomicUsize>,
}

impl Render for Child {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders.fetch_add(1, Ordering::Relaxed);
        let label = format!("painted {}", self.count);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("increment")
                    .w(px(100.))
                    .h(px(40.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.count += 1;
                        cx.notify();
                    }))
                    .agent_node(Role::Button, format!("count {}", self.count)),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        agent_paint_node(Role::Text, label.clone(), bounds, window, cx);
                    },
                )
                .w(px(100.))
                .h(px(40.)),
            )
    }
}

struct Parent(Entity<Child>);

impl Render for Parent {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        cached_view(self.0.clone(), StyleRefinement::default().size_full())
    }
}

#[test]
fn cached_nodes_survive_parent_frames_and_update_on_child_notifications() {
    let renders = Arc::new(AtomicUsize::new(0));
    let observed = renders.clone();
    let mut harness = Harness::headless(
        Config::default(),
        Arc::new(move |_: &mut Window, cx: &mut App| {
            let child = cx.new(|_| Child {
                count: 0,
                renders: observed.clone(),
            });
            cx.new(|_| Parent(child)).into()
        }),
    )
    .unwrap();
    let run = |harness: &mut Harness, code: &str| {
        let result = harness.exec(code, Duration::from_secs(10));
        assert_eq!(result.error, None, "{}", result.stdout);
        result.result
    };
    run(&mut harness, "app.frames(3)");
    let initial = renders.load(Ordering::Relaxed);
    let nodes = run(
        &mut harness,
        r#"
        app.frames(10);
        app.snapshot().nodes
    "#,
    );
    assert_eq!(
        renders.load(Ordering::Relaxed),
        initial,
        "parent frames rebuilt the child"
    );
    let nodes = nodes.as_array().unwrap();
    for label in ["count 0", "painted 0"] {
        assert_eq!(
            nodes.iter().filter(|n| n["label"] == label).count(),
            1,
            "{nodes:?}"
        );
    }
    for (id, node) in nodes.iter().enumerate() {
        assert_eq!(node["id"].as_u64(), Some(id as u64));
    }
    let updated = run(
        &mut harness,
        r#"
        app.click(app.snapshot().find({role: "button", label: "count 0"}));
        app.frames(3);
        app.snapshot().nodes.map(n => n.label)
    "#,
    );
    assert!(renders.load(Ordering::Relaxed) > initial);
    let labels = updated.as_array().unwrap();
    assert!(labels.iter().any(|v| v == "count 1"), "{labels:?}");
    assert!(labels.iter().any(|v| v == "painted 1"), "{labels:?}");
    assert!(!labels.iter().any(|v| v == "count 0" || v == "painted 0"));
}
