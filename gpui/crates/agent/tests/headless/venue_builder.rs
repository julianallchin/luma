#![cfg(feature = "app")]
//! The builder's inline height row, on a bespoke view.
//!
//! This one stays in Rust: it mounts its own gpui view around
//! `luma_ui::float::inline_field_row` and records the preview and commit
//! callbacks, which no script can observe. The stage page's own tests are
//! `tests/js/headless/venue_builder.test.js`.

#[test]
fn height_row_is_inline_and_drag_previews_before_one_commit() {
    use gpui::{prelude::*, App, Context, Render, Window};
    use std::sync::{Arc, Mutex};
    struct Probe(Arc<Mutex<Vec<(bool, f64)>>>);
    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            use luma_ui::node::{Instrument, Role};
            let commit = self.0.clone();
            let preview = self.0.clone();
            gpui::div().p(gpui::px(20.)).w(gpui::px(260.)).child(
                luma_ui::float::inline_field_row(
                    "Height",
                    luma_ui::scrub_number::ScrubNumber::new(
                        "height-probe",
                        1.,
                        0.5..=10.,
                        0.01,
                        90.,
                        "m",
                        move |value, _, _| commit.lock().unwrap().push((false, value)),
                    )
                    .on_preview(move |value, _, _| preview.lock().unwrap().push((true, value))),
                )
                .agent_node(Role::Row, "Height property"),
            )
        }
    }
    let samples = Arc::new(Mutex::new(Vec::new()));
    let root_samples = samples.clone();
    let root: gpui_agent::RootFactory = Arc::new(move |_: &mut Window, cx: &mut App| {
        cx.new(|_| Probe(root_samples.clone())).into()
    });
    let mut harness = gpui_agent::Harness::headless(gpui_agent::Config::default(), root).unwrap();
    let out = harness.exec(r#"
        app.frames(3);
        const row = app.snapshot().find({role:"row", label:"Height property"}).bounds;
        const value = app.snapshot().findAll({role:"slider"}).find(n => n.label.startsWith("height-probe"));
        const bounds = value.bounds;
        app.drag(value, {dx:80,dy:0}, {steps:8});
        ({row,bounds})
    "#, std::time::Duration::from_secs(30));
    assert_eq!(out.error, None, "{}", out.stdout);
    let row = &out.result["row"];
    let value = &out.result["bounds"];
    assert!((row["height"].as_f64().unwrap() - value["height"].as_f64().unwrap()).abs() < 2.);
    let samples = samples.lock().unwrap();
    assert!(
        samples.len() > 2,
        "drag must preview intermediate values: {samples:?}"
    );
    assert!(samples[..samples.len() - 1]
        .iter()
        .all(|(preview, _)| *preview));
    assert_eq!(samples.last(), Some(&(false, 1.2)));
}
