//! Pixel proof for the real backdrop primitive used by the shared dialog host.
#![cfg(feature = "pixel")]

use std::sync::Arc;
use std::time::Duration;
use std::{fs, path::PathBuf};

use gpui::{div, prelude::*, px, AnyView, App, Context, Render, Window};
use gpui_agent::{Config, Harness, Mode, GPU_LIVENESS_TIMEOUT};
use luma_ui::node::{Instrument, Role};
use serde_json::Value;

struct BlurProof;

fn stripes() -> gpui::Div {
    div().size_full().flex().children((0..19).map(|index| {
        div()
            .w(px(16.0))
            .h_full()
            .flex_none()
            .bg(if index % 2 == 0 {
                gpui::black()
            } else {
                gpui::white()
            })
    }))
}

fn sample(label: &'static str, blur: Option<f32>) -> impl IntoElement {
    div()
        .relative()
        .w(px(300.0))
        .h(px(240.0))
        .overflow_hidden()
        .child(stripes())
        .when_some(blur, |band, radius| {
            band.child(div().absolute().inset_0().child(luma_ui::dialog::frosted(
                0.0,
                radius,
                div().size_full(),
            )))
        })
        .agent_node(Role::Card, label)
}

fn content_sample(label: &'static str, blur: f32, scale: f32) -> impl IntoElement {
    div()
        .relative()
        .w(px(300.0))
        .h(px(240.0))
        .overflow_hidden()
        .bg(gpui::rgb(0x7a2030))
        .child(luma_ui::dialog::filtered(blur, scale, stripes()))
        .agent_node(Role::Card, label)
}

impl Render for BlurProof {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(24.0))
            .bg(gpui::rgb(0x202020))
            .child(
                div()
                    .flex()
                    .gap(px(24.0))
                    .child(sample("Unblurred stripes", None))
                    .child(sample(
                        "Card blur stripes",
                        Some(luma_ui::dialog::CARD_BLUR),
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap(px(24.0))
                    .child(content_sample("Sharp content layer", 0.0, 1.0))
                    .child(content_sample("Blurred content layer", 18.0, 1.0))
                    .child(content_sample("Scaled content layer", 0.0, 0.72)),
            )
    }
}

fn harness() -> Harness {
    let root: gpui_agent::RootFactory =
        Arc::new(|_: &mut Window, cx: &mut App| -> AnyView { cx.new(|_| BlurProof).into() });
    Harness::headless(
        Config {
            mode: Mode::Pixel,
            call_timeout: GPU_LIVENESS_TIMEOUT,
            ..Config::default()
        },
        root,
    )
    .expect("failed to start the pixel harness")
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, GPU_LIVENESS_TIMEOUT);
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

fn pixels(path: &str) -> image::RgbaImage {
    image::open(path)
        .unwrap_or_else(|error| panic!("could not read {path}: {error}"))
        .to_rgba8()
}

fn capture_dir() -> PathBuf {
    let dir = PathBuf::from(
        std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-dialog-blur".into()),
    );
    fs::create_dir_all(&dir).expect("could not create dialog capture directory");
    dir
}

fn preserve(source: &str, name: &str) {
    let destination = capture_dir().join(name);
    fs::copy(source, &destination)
        .unwrap_or_else(|error| panic!("could not preserve {}: {error}", destination.display()));
    println!("dialog blur capture {}", destination.display());
}

/// Mean horizontal high-frequency energy, excluding the outer eight pixels
/// where crop/background antialiasing can contribute unrelated edges.
fn edge_energy(image: &image::RgbaImage) -> f32 {
    let mut total = 0u64;
    let mut count = 0u64;
    for y in 8..image.height().saturating_sub(8) {
        for x in 9..image.width().saturating_sub(8) {
            let left = image.get_pixel(x - 1, y);
            let here = image.get_pixel(x, y);
            total += (0..3)
                .map(|channel| u64::from(left[channel].abs_diff(here[channel])))
                .sum::<u64>();
            count += 3;
        }
    }
    total as f32 / count.max(1) as f32
}

#[test]
fn the_card_frost_and_the_filtered_layer_reduce_real_pixel_edges() {
    let mut harness = harness();
    let shots = run(
        &mut harness,
        r#"
        const capture = (label) => app.screenshot({
            node: app.snapshot().find({ role: "card", label })
        }).path;
        ({
            full: app.screenshot().path,
            plain: capture("Unblurred stripes"),
            card: capture("Card blur stripes"),
            sharpContent: capture("Sharp content layer"),
            blurredContent: capture("Blurred content layer"),
            scaledContent: capture("Scaled content layer"),
        })
        "#,
    );
    preserve(shots["full"].as_str().unwrap(), "dialog-blur-full.png");
    preserve(shots["plain"].as_str().unwrap(), "dialog-blur-plain.png");
    preserve(shots["card"].as_str().unwrap(), "dialog-blur-card.png");
    preserve(
        shots["sharpContent"].as_str().unwrap(),
        "content-filter-sharp.png",
    );
    preserve(
        shots["blurredContent"].as_str().unwrap(),
        "content-filter-blurred.png",
    );
    preserve(
        shots["scaledContent"].as_str().unwrap(),
        "content-filter-scaled.png",
    );
    let plain = edge_energy(&pixels(shots["plain"].as_str().unwrap()));
    let card = edge_energy(&pixels(shots["card"].as_str().unwrap()));
    let sharp_content = edge_energy(&pixels(shots["sharpContent"].as_str().unwrap()));
    let blurred_content = edge_energy(&pixels(shots["blurredContent"].as_str().unwrap()));
    let scaled_content = pixels(shots["scaledContent"].as_str().unwrap());
    println!("dialog blur edge energy: plain={plain:.3}, card={card:.3}");

    // The card's own frost is the ONLY backdrop blur the modal plane paints
    // now — the plane behind it is a plain tint. So this asserts the one blur
    // that is left really samples what is behind it, rather than comparing two
    // strengths of a plane blur that no longer exists.
    assert!(
        card < plain * 0.6,
        "card blur did not reduce edges: {plain} -> {card}"
    );
    assert!(
        blurred_content < sharp_content * 0.6,
        "filtered subtree did not transition blur -> sharp: {sharp_content} -> {blurred_content}"
    );
    let corner = scaled_content.get_pixel(4, 4);
    assert!(
        corner[0] > 90 && corner[0] > corner[1] * 2 && corner[0] > corner[2],
        "scaled filtered layer did not expose its red parent at the corner: {corner:?}"
    );
}
