//! Outside-in geometry and pixel proof for the shared morph-dialog reducer.

#![cfg(feature = "pixel")]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{actions, div, px, AnyView, App, Context, FocusHandle, Render, Window};
use gpui_agent::{Config, Harness, Mode};
use luma_ui::dialog::morph::{
    self, ContentMode, LayerPose, LayerRole, MorphDialog, MorphSample, MorphSize, MorphTransition,
    RouteDescriptor,
};
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use serde_json::Value;

const A_WIDTH: f32 = 340.0;
const A_HEIGHT: f32 = 220.0;

actions!(
    dialog_morph_proof,
    [
        GoA,
        GoB,
        GoC,
        GoBScale,
        GoBCrossFade,
        GoBCustom,
        ReduceMotion,
        HoldMeasurements,
        GoCIntrinsic,
        ResolveStaleB,
        ReleaseMeasurements
    ]
);

fn custom_transition(role: LayerRole, progress: f32) -> LayerPose {
    LayerPose {
        x: match role {
            LayerRole::Outgoing => 10.0 * progress,
            LayerRole::Incoming => -10.0 * (1.0 - progress),
        },
        opacity: match role {
            LayerRole::Outgoing => 1.0 - progress,
            LayerRole::Incoming => progress,
        },
        blur: 0.0,
        scale: 1.0,
    }
}

struct Proof {
    morph: MorphDialog<&'static str>,
    trap: FocusHandle,
    a_focus: FocusHandle,
    b_focus: FocusHandle,
    measurements_held: bool,
    held_measurement: Option<luma_ui::dialog::morph::MeasurementToken>,
}

impl Proof {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            morph: MorphDialog::new(
                RouteDescriptor::exact("a", A_WIDTH, A_HEIGHT),
                MorphSize::new(A_WIDTH, A_HEIGHT),
            ),
            trap: cx.focus_handle(),
            a_focus: cx.focus_handle().tab_stop(true),
            b_focus: cx.focus_handle().tab_stop(true),
            measurements_held: false,
            held_measurement: None,
        }
    }

    fn request_b(&mut self, transition: MorphTransition, cx: &mut Context<Self>) {
        self.morph.request(
            RouteDescriptor::intrinsic("b", MorphSize::new(560.0, 400.0))
                .with_transition(transition),
            Instant::now(),
            luma_ui::motion::reduced_motion(cx),
        );
        if self.measurements_held {
            self.held_measurement = self.morph.pending_measure().map(|pending| pending.token);
        }
        cx.notify();
    }

    fn request_a(&mut self, cx: &mut Context<Self>) {
        self.morph.request(
            RouteDescriptor::exact("a", A_WIDTH, A_HEIGHT),
            Instant::now(),
            luma_ui::motion::reduced_motion(cx),
        );
        cx.notify();
    }

    fn request_c(&mut self, cx: &mut Context<Self>) {
        self.morph.request(
            RouteDescriptor::exact("c", 460.0, 280.0),
            Instant::now(),
            luma_ui::motion::reduced_motion(cx),
        );
        cx.notify();
    }

    fn request_c_intrinsic(&mut self, cx: &mut Context<Self>) {
        self.morph.request(
            RouteDescriptor::intrinsic("c", MorphSize::new(500.0, 320.0)),
            Instant::now(),
            luma_ui::motion::reduced_motion(cx),
        );
        cx.notify();
    }
}

fn stripes() -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .left_0()
        .flex()
        .children((0..22).map(|index| {
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

fn route_a(
    mode: ContentMode,
    app: &gpui::Entity<Proof>,
    focus: &FocusHandle,
    focused: bool,
) -> gpui::AnyElement {
    let requested = app.clone();
    div()
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .justify_end()
        .bg(gpui::rgb(0x202020))
        .child(stripes())
        .child(
            div()
                .relative()
                .w_full()
                .px(px(24.0))
                .text_color(gpui::white())
                .child(
                    "Responsive copy wraps at the route width and must not reflow while the card grows.",
                )
                .agent_node(Role::Row, "A wrapping copy"),
        )
        .child(
            div()
                .relative()
                .h(px(66.0))
                .flex_none()
                .px(px(24.0))
                .flex()
                .items_center()
                .justify_between()
                .text_color(gpui::white())
                .child("Route A")
                .when(mode == ContentMode::Interactive, |footer| {
                    footer.child(
                        div()
                            .id("next")
                            .px(px(12.0))
                            .py(px(6.0))
                            .bg(gpui::rgb(0x3f3f46))
                            .track_focus(focus)
                            .on_click(move |_, _, cx| {
                                requested.update(cx, |proof, cx| {
                                    proof.request_b(MorphTransition::Right, cx)
                                })
                            })
                            .child("Next")
                            .agent_node(Role::Button, "Next")
                            .agent_focused(focused),
                    )
                }),
        )
        .agent_node(Role::Card, "Route A")
        .into_any_element()
}

fn route_b(
    mode: ContentMode,
    app: &gpui::Entity<Proof>,
    focus: &FocusHandle,
    focused: bool,
) -> gpui::AnyElement {
    let requested = app.clone();
    div()
        .w(px(520.0))
        .flex()
        .flex_col()
        .bg(gpui::rgb(0x20252c))
        .child(
            div()
                .h(px(84.0))
                .flex_none()
                .px(px(24.0))
                .flex()
                .items_center()
                .text_color(gpui::white())
                .child("Measured route B"),
        )
        .child(
            div()
                .h(px(170.0))
                .flex_none()
                .mx(px(24.0))
                .bg(gpui::rgb(0x334155)),
        )
        .child(
            div()
                .h(px(66.0))
                .flex_none()
                .px(px(24.0))
                .flex()
                .items_center()
                .justify_end()
                .when(mode == ContentMode::Interactive, |footer| {
                    footer.child(
                        div()
                            .id("back")
                            .px(px(12.0))
                            .py(px(6.0))
                            .bg(gpui::rgb(0x475569))
                            .track_focus(focus)
                            .on_click(move |_, _, cx| {
                                requested.update(cx, |proof, cx| proof.request_a(cx))
                            })
                            .child("Back")
                            .agent_node(Role::Button, "Back")
                            .agent_focused(focused),
                    )
                }),
        )
        .agent_node(Role::Card, "Route B")
        .into_any_element()
}

fn route_c() -> gpui::AnyElement {
    div()
        .w(px(460.0))
        .h(px(280.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::rgb(0x3b2748))
        .text_color(gpui::white())
        .child("Replacement route C")
        .agent_node(Role::Card, "Route C")
        .into_any_element()
}

fn route_content(
    key: &&'static str,
    mode: ContentMode,
    app: &gpui::Entity<Proof>,
    a_focus: &FocusHandle,
    b_focus: &FocusHandle,
    a_focused: bool,
    b_focused: bool,
) -> gpui::AnyElement {
    match *key {
        "a" => route_a(mode, app, a_focus, a_focused),
        "b" => route_b(mode, app, b_focus, b_focused),
        "c" => route_c(),
        _ => unreachable!(),
    }
}

impl Render for Proof {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        if self.morph.tick(now, luma_ui::motion::reduced_motion(cx)) {
            window.request_animation_frame();
        }
        if let Some(key) = self.morph.take_focus_after_commit() {
            let focus = if key == "a" {
                self.a_focus.clone()
            } else {
                self.b_focus.clone()
            };
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }

        let sample: MorphSample<&'static str> = self.morph.sample(now);
        if sample.animating {
            // The outgoing control is deliberately unmounted for the whole
            // flight. Keep focus on the modal scope instead of leaving a
            // dangling route handle; named actions still reach the proof and
            // the target does not receive focus before commit.
            window.focus(&self.trap, cx);
        } else if !self.a_focus.is_focused(window) && !self.b_focus.is_focused(window) {
            let focus = if sample.layers[0].key == "a" {
                &self.a_focus
            } else {
                &self.b_focus
            };
            window.focus(focus, cx);
        }
        let app = cx.entity();
        let card_app = app.clone();
        let a_focus = self.a_focus.clone();
        let b_focus = self.b_focus.clone();
        let a_focused = self.a_focus.is_focused(window);
        let b_focused = self.b_focus.is_focused(window);
        let card = morph::card(&sample, "Morph container", move |key, mode| {
            route_content(
                key, mode, &card_app, &a_focus, &b_focus, a_focused, b_focused,
            )
        });
        let host = luma_ui::dialog::host(
            "morph-proof-host",
            window.viewport_size(),
            px(220.0),
            &self.trap,
            self.trap.contains_focused(window, cx),
            "Morph host",
            luma_ui::dialog::ScrimDismiss::Disabled,
            card,
        );

        let pending = (!self.measurements_held)
            .then(|| {
                self.morph
                    .pending_measure()
                    .map(|pending| (pending.key, pending.token))
            })
            .flatten();
        div()
            .size_full()
            .relative()
            // Actions dispatch only along the tracked focus path. The proof's
            // controls live inside this scope; without it, named mutations are
            // valid actions but intentionally unhandled by GPUI.
            .track_focus(&self.trap)
            .bg(gpui::rgb(0x111318))
            .on_action(cx.listener(|this, _: &GoA, _, cx| this.request_a(cx)))
            .on_action(
                cx.listener(|this, _: &GoB, _, cx| this.request_b(MorphTransition::Right, cx)),
            )
            .on_action(cx.listener(|this, _: &GoC, _, cx| this.request_c(cx)))
            .on_action(
                cx.listener(|this, _: &GoBScale, _, cx| this.request_b(MorphTransition::Scale, cx)),
            )
            .on_action(cx.listener(|this, _: &GoBCrossFade, _, cx| {
                this.request_b(MorphTransition::CrossFade, cx)
            }))
            .on_action(cx.listener(|this, _: &GoBCustom, _, cx| {
                this.request_b(MorphTransition::Custom(custom_transition), cx)
            }))
            .on_action(cx.listener(|_, _: &ReduceMotion, _, cx| {
                luma_ui::motion::set_reduced_motion(cx, true);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &HoldMeasurements, _, cx| {
                this.measurements_held = true;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &GoCIntrinsic, _, cx| this.request_c_intrinsic(cx)))
            .on_action(cx.listener(|this, _: &ResolveStaleB, _, cx| {
                if let Some(token) = this.held_measurement.take() {
                    this.morph.resolve_intrinsic(
                        token,
                        MorphSize::new(520.0, 320.0),
                        Instant::now(),
                        luma_ui::motion::reduced_motion(cx),
                    );
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ReleaseMeasurements, _, cx| {
                this.measurements_held = false;
                cx.notify();
            }))
            .child(stripes())
            .child(host)
            .when_some(pending, |root, (key, token)| {
                let measured_app = app.clone();
                let measured_route = if key == "b" {
                    route_b(ContentMode::PaintOnly, &app, &self.b_focus, false)
                } else {
                    route_c()
                };
                root.child(morph::premeasure(
                    div().absolute().top_0().left_0().child(measured_route),
                    move |size, _, cx| {
                        measured_app.update(cx, |proof, cx| {
                            proof.morph.resolve_intrinsic(
                                token,
                                MorphSize::new(f32::from(size.width), f32::from(size.height)),
                                Instant::now(),
                                luma_ui::motion::reduced_motion(cx),
                            );
                            cx.notify();
                        });
                    },
                ))
            })
    }
}

fn harness() -> Harness {
    std::env::set_var("LUMA_MOTION", "on");
    std::env::set_var("LUMA_MOTION_SCALE", "10");
    let root: gpui_agent::RootFactory = Arc::new(|window: &mut Window, cx: &mut App| -> AnyView {
        gpui_component::init(cx);
        luma_ui::motion::set_reduced_motion(cx, false);
        let proof = cx.new(Proof::new);
        cx.new(|cx| gpui_component::Root::new(proof, window, cx).bordered(false))
            .into()
    });
    Harness::headless(
        Config {
            mode: Mode::Pixel,
            window_size: gpui::size(px(800.0), px(600.0)),
            call_timeout: Duration::from_secs(30),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start morph pixel harness")
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(30));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

fn capture_dir() -> PathBuf {
    let directory = PathBuf::from(
        std::env::var("LUMA_SHOTS").unwrap_or_else(|_| "/tmp/luma-dialog-morph-review".into()),
    );
    fs::create_dir_all(&directory).unwrap();
    directory
}

fn preserve(source: &str, name: &str) -> PathBuf {
    let destination = capture_dir().join(name);
    fs::copy(source, &destination).unwrap();
    println!("dialog morph capture {}", destination.display());
    destination
}

fn crop(path: &PathBuf, bounds: &Value) -> image::RgbaImage {
    let image = image::open(path).unwrap().to_rgba8();
    let scale = image.width() as f64 / 800.0;
    let number = |key: &str| bounds[key].as_f64().unwrap();
    image::imageops::crop_imm(
        &image,
        (number("x") * scale).round() as u32,
        (number("y") * scale).round() as u32,
        (number("width") * scale).round() as u32,
        (number("height") * scale).round() as u32,
    )
    .to_image()
}

fn edge_energy(image: &image::RgbaImage) -> f32 {
    let mut total = 0_u64;
    let mut count = 0_u64;
    for y in 8..image.height().saturating_sub(8) {
        for x in 9..image.width().saturating_sub(8) {
            for channel in 0..3 {
                total += u64::from(
                    image.get_pixel(x - 1, y)[channel].abs_diff(image.get_pixel(x, y)[channel]),
                );
                count += 1;
            }
        }
    }
    total as f32 / count.max(1) as f32
}

#[test]
fn intrinsic_target_morphs_both_axes_while_outgoing_content_blurs() {
    let mut harness = harness();
    let out = run(
        &mut harness,
        r#"
        app.frames(2, { waitMs: 2100 });
        const openingShot = app.snapshot();
        const opening = openingShot.find({ role: "card", label: "Morph container" });
        const openingWrap = openingShot.find({ role: "row", label: "A wrapping copy" });
        const sharp = app.screenshot().path;
        app.click(app.snapshot().find({ role: "button", label: "Next" }));
        app.frames(3, { waitMs: 70 });
        const middleShot = app.snapshot();
        const middle = middleShot.find({ role: "card", label: "Morph container" });
        const middleWrap = middleShot.find({ role: "row", label: "A wrapping copy" });
        const midBlur = app.screenshot().path;
        const nextDuring = middleShot.find({ role: "button", label: "Next" });
        const backDuring = middleShot.find({ role: "button", label: "Back" });
        app.frames(2, { waitMs: 2100 });
        const settledShot = app.snapshot();
        const settled = settledShot.find({ role: "card", label: "Morph container" });
        const back = settledShot.find({ role: "button", label: "Back" });
        ({ opening: opening.bounds, openingWrap: openingWrap.bounds,
           middle: middle.bounds, middleWrap: middleWrap.bounds, settled: settled.bounds,
           nextDuring, backDuring, back, sharp, midBlur })
        "#,
    );

    let sharp_frame = preserve(
        out["sharp"].as_str().unwrap(),
        "dialog-morph-sharp-full.png",
    );
    let blurred_frame = preserve(
        out["midBlur"].as_str().unwrap(),
        "dialog-morph-mid-full.png",
    );
    let sharp = crop(&sharp_frame, &out["opening"]);
    let blurred = crop(&blurred_frame, &out["middle"]);
    let sharp_crop = capture_dir().join("dialog-morph-sharp-a.png");
    let blurred_crop = capture_dir().join("dialog-morph-mid-a.png");
    sharp.save(&sharp_crop).unwrap();
    blurred.save(&blurred_crop).unwrap();
    println!("dialog morph capture {}", sharp_crop.display());
    println!("dialog morph capture {}", blurred_crop.display());
    let sharp_energy = edge_energy(&sharp);
    let blurred_energy = edge_energy(&blurred);
    println!("dialog morph edge energy sharp={sharp_energy:.3} mid={blurred_energy:.3}");

    let width = |at: &str| out[at]["width"].as_f64().unwrap();
    let height = |at: &str| out[at]["height"].as_f64().unwrap();
    assert_eq!((width("opening"), height("opening")), (340.0, 220.0));
    assert_eq!(
        (
            out["openingWrap"]["width"].as_f64().unwrap(),
            out["openingWrap"]["height"].as_f64().unwrap(),
        ),
        (
            out["middleWrap"]["width"].as_f64().unwrap(),
            out["middleWrap"]["height"].as_f64().unwrap(),
        ),
        "responsive route reflowed with the live container: {out:#}"
    );
    assert!(
        width("middle") > 340.0 && width("middle") < 520.0,
        "{out:#}"
    );
    assert!(
        height("middle") > 220.0 && height("middle") < 320.0,
        "{out:#}"
    );
    assert_eq!((width("settled"), height("settled")), (520.0, 320.0));
    assert!(
        out["nextDuring"].is_null(),
        "outgoing route kept input: {out:#}"
    );
    assert!(
        out["backDuring"].is_null(),
        "target admitted input before commit: {out:#}"
    );
    assert_eq!(
        out["back"]["focused"], true,
        "focus did not wait for commit: {out:#}"
    );

    assert!(
        blurred_energy < sharp_energy * 0.75,
        "outgoing content did not visibly blur: {sharp_energy:.3} -> {blurred_energy:.3}"
    );
}

#[test]
fn production_card_reversal_and_replacement_preserve_the_visible_rect_then_prune() {
    let mut harness = harness();
    let out = run(
        &mut harness,
        r#"
        app.frames(2, { waitMs: 2100 });
        app.click(app.snapshot().find({ role: "button", label: "Next" }));
        app.frames(3, { waitMs: 70 });
        const beforeReverse = app.snapshot().find({ role: "card", label: "Morph container" }).bounds;
        app.action("dialog_morph_proof::GoA");
        const afterReverse = app.snapshot().find({ role: "card", label: "Morph container" }).bounds;
        app.frames(2, { waitMs: 2100 });
        const reversed = app.snapshot();

        app.click(reversed.find({ role: "button", label: "Next" }));
        app.frames(3, { waitMs: 70 });
        const beforeReplace = app.snapshot().find({ role: "card", label: "Morph container" }).bounds;
        app.action("dialog_morph_proof::GoC");
        const replacement = app.snapshot();
        const afterReplace = replacement.find({ role: "card", label: "Morph container" }).bounds;
        app.frames(2, { waitMs: 2100 });
        const settled = app.snapshot();
        ({ beforeReverse, afterReverse,
           reversedA: reversed.find({ role: "card", label: "Route A" }),
           reversedB: reversed.find({ role: "card", label: "Route B" }),
           beforeReplace, afterReplace,
           replacementA: replacement.find({ role: "card", label: "Route A" }),
           replacementB: replacement.find({ role: "card", label: "Route B" }),
           replacementC: replacement.find({ role: "card", label: "Route C" }),
           replacementNext: replacement.find({ role: "button", label: "Next" }),
           replacementBack: replacement.find({ role: "button", label: "Back" }),
           settledContainer: settled.find({ role: "card", label: "Morph container" }).bounds,
           settledA: settled.find({ role: "card", label: "Route A" }),
           settledB: settled.find({ role: "card", label: "Route B" }),
           settledC: settled.find({ role: "card", label: "Route C" }) })
        "#,
    );

    for (before, after) in [
        (&out["beforeReverse"], &out["afterReverse"]),
        (&out["beforeReplace"], &out["afterReplace"]),
    ] {
        for axis in ["x", "y", "width", "height"] {
            let delta = (before[axis].as_f64().unwrap() - after[axis].as_f64().unwrap()).abs();
            assert!(delta <= 2.0, "{axis} jumped by {delta}: {out:#}");
        }
    }
    assert!(!out["reversedA"].is_null(), "{out:#}");
    assert!(out["reversedB"].is_null(), "{out:#}");
    assert!(!out["replacementA"].is_null(), "{out:#}");
    assert!(!out["replacementB"].is_null(), "{out:#}");
    assert!(!out["replacementC"].is_null(), "{out:#}");
    assert!(out["replacementNext"].is_null(), "{out:#}");
    assert!(out["replacementBack"].is_null(), "{out:#}");
    assert_eq!(
        (
            out["settledContainer"]["width"].as_f64().unwrap(),
            out["settledContainer"]["height"].as_f64().unwrap(),
        ),
        (460.0, 280.0)
    );
    assert!(out["settledA"].is_null(), "{out:#}");
    assert!(out["settledB"].is_null(), "{out:#}");
    assert!(!out["settledC"].is_null(), "{out:#}");
}

#[test]
fn production_card_reduced_motion_toggle_snaps_the_active_intrinsic_target() {
    let mut harness = harness();
    let out = run(
        &mut harness,
        r#"
        app.frames(2, { waitMs: 2100 });
        app.click(app.snapshot().find({ role: "button", label: "Next" }));
        app.frames(3, { waitMs: 70 });
        const middle = app.snapshot().find({ role: "card", label: "Morph container" }).bounds;
        app.action("dialog_morph_proof::ReduceMotion");
        app.frames(2);
        const settled = app.snapshot();
        ({ middle,
           container: settled.find({ role: "card", label: "Morph container" }).bounds,
           routeA: settled.find({ role: "card", label: "Route A" }),
           routeB: settled.find({ role: "card", label: "Route B" }),
           back: settled.find({ role: "button", label: "Back" }) })
        "#,
    );
    assert!(out["middle"]["width"].as_f64().unwrap() < 520.0, "{out:#}");
    assert_eq!(
        (
            out["container"]["width"].as_f64().unwrap(),
            out["container"]["height"].as_f64().unwrap(),
        ),
        (520.0, 320.0)
    );
    assert!(out["routeA"].is_null(), "{out:#}");
    assert!(!out["routeB"].is_null(), "{out:#}");
    assert_eq!(out["back"]["focused"], true, "{out:#}");
}

#[test]
fn production_card_ignores_a_stale_intrinsic_result_after_target_replacement() {
    let mut harness = harness();
    let out = run(
        &mut harness,
        r#"
        app.frames(2, { waitMs: 2100 });
        app.action("dialog_morph_proof::HoldMeasurements");
        app.action("dialog_morph_proof::GoB");
        app.action("dialog_morph_proof::GoCIntrinsic");
        app.action("dialog_morph_proof::ResolveStaleB");
        app.frames(2);
        const held = app.snapshot();
        app.action("dialog_morph_proof::ReleaseMeasurements");
        app.frames(3, { waitMs: 70 });
        const middle = app.snapshot();
        app.frames(2, { waitMs: 2100 });
        const settled = app.snapshot();
        ({ heldContainer: held.find({ role: "card", label: "Morph container" }).bounds,
           heldA: held.find({ role: "card", label: "Route A" }),
           heldB: held.find({ role: "card", label: "Route B" }),
           middleB: middle.find({ role: "card", label: "Route B" }),
           middleC: middle.find({ role: "card", label: "Route C" }),
           settledContainer: settled.find({ role: "card", label: "Morph container" }).bounds,
           settledA: settled.find({ role: "card", label: "Route A" }),
           settledB: settled.find({ role: "card", label: "Route B" }),
           settledC: settled.find({ role: "card", label: "Route C" }) })
        "#,
    );
    assert_eq!(
        (
            out["heldContainer"]["width"].as_f64().unwrap(),
            out["heldContainer"]["height"].as_f64().unwrap(),
        ),
        (340.0, 220.0),
        "stale B committed while C still awaited measurement: {out:#}"
    );
    assert!(!out["heldA"].is_null(), "{out:#}");
    assert!(out["heldB"].is_null(), "{out:#}");
    assert!(out["middleB"].is_null(), "{out:#}");
    assert!(!out["middleC"].is_null(), "{out:#}");
    assert_eq!(
        (
            out["settledContainer"]["width"].as_f64().unwrap(),
            out["settledContainer"]["height"].as_f64().unwrap(),
        ),
        (460.0, 280.0)
    );
    assert!(out["settledA"].is_null(), "{out:#}");
    assert!(out["settledB"].is_null(), "{out:#}");
    assert!(!out["settledC"].is_null(), "{out:#}");
}

#[test]
fn scale_crossfade_and_custom_transitions_render_through_the_production_card() {
    for action in ["GoBScale", "GoBCrossFade", "GoBCustom"] {
        let mut harness = harness();
        let out = run(
            &mut harness,
            &format!(
                r#"
                app.frames(2, {{ waitMs: 2100 }});
                app.action("dialog_morph_proof::{action}");
                app.frames(3, {{ waitMs: 70 }});
                const middle = app.snapshot();
                const pixel = app.screenshot().path;
                app.frames(2, {{ waitMs: 2100 }});
                const settled = app.snapshot();
                ({{
                   middleA: middle.find({{ role: "card", label: "Route A" }}),
                   middleB: middle.find({{ role: "card", label: "Route B" }}),
                   middleNext: middle.find({{ role: "button", label: "Next" }}),
                   middleBack: middle.find({{ role: "button", label: "Back" }}),
                   settledA: settled.find({{ role: "card", label: "Route A" }}),
                   settledB: settled.find({{ role: "card", label: "Route B" }}),
                   container: settled.find({{ role: "card", label: "Morph container" }}).bounds,
                   pixel }})
                "#
            ),
        );
        preserve(
            out["pixel"].as_str().unwrap(),
            &format!("dialog-morph-{action}-mid.png"),
        );
        assert!(!out["middleA"].is_null(), "{action}: {out:#}");
        assert!(!out["middleB"].is_null(), "{action}: {out:#}");
        assert!(out["middleNext"].is_null(), "{action}: {out:#}");
        assert!(out["middleBack"].is_null(), "{action}: {out:#}");
        assert!(out["settledA"].is_null(), "{action}: {out:#}");
        assert!(!out["settledB"].is_null(), "{action}: {out:#}");
        assert_eq!(
            (
                out["container"]["width"].as_f64().unwrap(),
                out["container"]["height"].as_f64().unwrap(),
            ),
            (520.0, 320.0),
            "{action}: {out:#}"
        );
    }
}
