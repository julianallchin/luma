//! Capture the shared action family together so panel and dialog styles can be reviewed.
#![cfg(feature = "pixel")]
use gpui::{div, prelude::*, px, size, AnyView, App, Context, Render, Window};
use gpui_agent::{Config, Harness, Mode, GPU_LIVENESS_TIMEOUT};
use luma_ui::node::{Instrument, Role};
use luma_ui::{button, float, ladder, Enabled};
use std::sync::Arc;

struct Buttons;
impl Render for Buttons {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(ladder::background())
            .text_color(ladder::foreground())
            .p(px(24.))
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(float::label("Toolbar actions"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        button("Play", Enabled::Yes)
                            .id("play")
                            .agent_node(Role::Button, "Play"),
                    )
                    .child(luma_ui::silkscreen("140.0 BPM"))
                    .child(
                        button("Beat grid correct", Enabled::Yes)
                            .id("approve")
                            .child(
                                gpui_component::Icon::new(luma_ui::icons::IconName::ThumbsUp)
                                    .size(px(14.)),
                            )
                            .agent_node(Role::Button, "Beat grid correct"),
                    )
                    .child(
                        button("Unavailable", Enabled::No)
                            .id("disabled")
                            .agent_node(Role::Button, "Unavailable"),
                    ),
            )
            .child(float::label("Toggles and menus"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(luma_ui::luma_toggle_group(
                        "Bars",
                        &["Bars", "Beats", "Seconds"],
                    ))
                    .child(luma_ui::luma_selector(
                        "Replace",
                        &["Replace", "Add", "Multiply"],
                    ))
                    .child(luma_ui::luma_dropdown("Actions", &["Duplicate", "Remove"])),
            )
            .child(float::label("Dialog actions"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(float::btn("Cancel", "cancel").id("cancel"))
                    .child(float::btn_primary("Save changes").id("save")),
            )
    }
}

#[test]
fn shared_buttons_use_comet_styles() {
    let root: gpui_agent::RootFactory =
        Arc::new(|_: &mut Window, cx: &mut App| -> AnyView { cx.new(|_| Buttons).into() });
    let mut harness = Harness::headless(
        Config {
            mode: Mode::Pixel,
            window_size: size(px(740.), px(310.)),
            call_timeout: GPU_LIVENESS_TIMEOUT,
            ..Config::default()
        },
        root,
    )
    .unwrap();
    let result = harness.exec(
        r#"app.frames(3); const shot = app.snapshot(); ({
            play: shot.find({role: "button", label: "Play"}),
            approve: shot.find({role: "button", label: "Beat grid correct"}),
            disabled: shot.find({role: "button", label: "Unavailable"})
        })"#,
        GPU_LIVENESS_TIMEOUT,
    );
    assert_eq!(result.error, None, "{}", result.stdout);
    let bounds = |name: &str| &result.result[name]["bounds"];
    for name in ["play", "approve", "disabled"] {
        assert!(bounds(name)["width"].as_f64().unwrap() > 0.);
        assert!(bounds(name)["height"].as_f64().unwrap() > 0.);
    }
    assert!(
        bounds("approve")["x"].as_f64().unwrap() + bounds("approve")["width"].as_f64().unwrap()
            <= bounds("disabled")["x"].as_f64().unwrap()
    );
    // GPUI's pinned platform supplies an offscreen renderer only on macOS.
    #[cfg(target_os = "macos")]
    {
        let capture = harness.exec("app.screenshot()", GPU_LIVENESS_TIMEOUT);
        assert_eq!(capture.error, None);
        let source = capture.result["path"].as_str().expect("screenshot path");
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../harness/shots/gpui/comet-buttons.png");
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::copy(source, &output).unwrap();
        println!("Button style capture: {}", output.display());
    }
}
