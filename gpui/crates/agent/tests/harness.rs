//! The harness driving a view of its own.
//!
//! Deliberately not `luma-app`: these tests are about the harness — frames,
//! staleness, determinism, deadlines — and pointing them at the real app would
//! make them depend on a database and on whatever the venue grid happens to
//! show. The view below is the smallest thing that has a button worth clicking.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    actions, canvas, div, point, px, size, uniform_list, AnyView, App, Bounds, Context,
    DispatchPhase, Empty, FocusHandle, HitboxBehavior, KeyBinding, PromptLevel, Render,
    ScrollWheelEvent, Window,
};
use gpui_agent::{Config, Harness, Mode};
use luma_ui::node::{agent_paint_node, AgentNode, Instrument, Role};
use serde_json::Value;

actions!(gpui_agent_test, [Bump]);

/// What a drag carries. Its type is the whole contract — gpui routes a drop to
/// the listener registered for that type.
struct Parcel;

// -- the view under test ------------------------------------------------------

struct Counter {
    count: usize,
    /// How many parcels have been dropped on the target.
    dropped: usize,
    /// Which answer a modal prompt came back with, once one has.
    answered: Option<usize>,
    /// Set on the button that deliberately blocks the app thread, so the
    /// timeout test has something that cannot answer in time.
    stall: Duration,
    /// gpui routes both actions and keystrokes along the path to the *focused*
    /// element, so a view with nothing focused cannot be driven by either.
    focus: FocusHandle,
    /// Accumulated wheel deltas, and whether the modifier was held. A wheel
    /// handler is reachable no other way, which is the point of the test.
    wheel: (f32, f32),
    zoomed: bool,
}

impl Counter {
    fn new(stall: Duration, cx: &mut Context<Self>) -> Self {
        Self {
            count: 0,
            dropped: 0,
            answered: None,
            stall,
            focus: cx.focus_handle(),
            wheel: (0., 0.),
            zoomed: false,
        }
    }
}

impl Render for Counter {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus.is_focused(window) {
            window.focus(&self.focus, cx);
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .key_context("Counter")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Bump, _, cx| {
                this.count += 1;
                cx.notify();
            }))
            .child(
                div()
                    .id("source")
                    .w(px(120.))
                    .h(px(40.))
                    .child("drag me")
                    .on_drag(Parcel, |_, _, _, cx| cx.new(|_| Empty))
                    .agent_node(Role::Card, "Source"),
            )
            .child(
                div()
                    .id("target")
                    .w(px(120.))
                    .h(px(40.))
                    .child("drop here")
                    .on_drop(cx.listener(|this, _: &Parcel, _, cx| {
                        this.dropped += 1;
                        cx.notify();
                    }))
                    .agent_node(Role::Card, "Target"),
            )
            .child(
                div()
                    .child(format!("dropped {}", self.dropped))
                    .agent_node(Role::Text, format!("dropped {}", self.dropped)),
            )
            .child(
                luma_ui::luma_button("Increment", false)
                    .id("increment")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.count += 1;
                        cx.notify();
                    }))
                    .agent_node(Role::Button, "Increment"),
            )
            .child(
                luma_ui::luma_button("Modal", false)
                    .id("modal")
                    .on_click(cx.listener(|_, _, window, cx| {
                        let answer = window.prompt(
                            PromptLevel::Info,
                            "Discard changes?",
                            None,
                            &["Discard", "Cancel"],
                            cx,
                        );
                        cx.spawn(async move |this, cx| {
                            let chosen = answer.await.ok();
                            this.update(cx, |this, cx| {
                                this.answered = chosen;
                                cx.notify();
                            })
                            .ok();
                        })
                        .detach();
                    }))
                    .agent_node(Role::Button, "Modal"),
            )
            .child(
                luma_ui::luma_button("Locked", true)
                    .id("locked")
                    .agent_node(Role::Button, "Locked")
                    .agent_disabled(true),
            )
            // A virtualized list: gpui only builds the rows in view, so the
            // snapshot names those and no others. That is the case the whole
            // frame-scoped-id rule exists for — row 90's id today is row 3's
            // id after a scroll.
            .child(
                div().h(px(64.)).overflow_hidden().child(
                    uniform_list("rows", 200, |range, _, _| {
                        range
                            .map(|index| {
                                div()
                                    .h(px(16.))
                                    .child(format!("row {index}"))
                                    .agent_node(Role::Row, format!("row {index}"))
                                    .into_any_element()
                            })
                            .collect()
                    })
                    .size_full(),
                ),
            )
            .child(
                luma_ui::luma_button("Stall", false)
                    .id("stall")
                    .on_click(cx.listener(|this, _, _, _| std::thread::sleep(this.stall)))
                    .agent_node(Role::Button, "Stall"),
            )
            // The label carries the value, which is what makes "the snapshot
            // differs" observable without reaching into the view.
            .child(
                div()
                    .child(format!("{}", self.count))
                    .agent_node(Role::Text, format!("count {}", self.count)),
            )
            .child(
                div()
                    .child(match self.answered {
                        Some(answer) => format!("answered {answer}"),
                        None => "unanswered".to_string(),
                    })
                    .agent_node(
                        Role::Text,
                        match self.answered {
                            Some(answer) => format!("answered {answer}"),
                            None => "unanswered".to_string(),
                        },
                    ),
            )
            .child(wheel_canvas(cx))
            .child(
                div()
                    .child(format!(
                        "wheel {:.0},{:.0} {}",
                        self.wheel.0,
                        self.wheel.1,
                        if self.zoomed { "zoom" } else { "pan" }
                    ))
                    .agent_node(
                        Role::Text,
                        format!(
                            "wheel {:.0},{:.0} {}",
                            self.wheel.0,
                            self.wheel.1,
                            if self.zoomed { "zoom" } else { "pan" }
                        ),
                    ),
            )
            .child(painted_canvas())
    }
}

/// A canvas that answers the wheel, scoped to its own hitbox the way the real
/// editors are — the shape `app.scroll` has to be able to reach.
fn wheel_canvas(cx: &mut Context<Counter>) -> impl IntoElement {
    let view = cx.entity();
    div().w(px(120.)).h(px(40.)).child(
        canvas(
            move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                let view = view.clone();
                let over = hitbox.clone();
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !over.is_hovered(window) {
                        return;
                    }
                    let delta = event.delta.pixel_delta(window.line_height());
                    let zoomed = event.modifiers.secondary() || event.modifiers.control;
                    view.update(cx, |this, cx| {
                        this.wheel.0 += f32::from(delta.x);
                        this.wheel.1 += f32::from(delta.y);
                        this.zoomed = zoomed;
                        cx.notify();
                    });
                });
            },
        )
        .size_full()
        .agent_node(Role::Card, "Wheel"),
    )
}

/// Two cards registered by hand from inside a clipped canvas — one within the
/// mask, one past its right edge.
///
/// This is the only way a node with empty bounds can reach the registry: gpui
/// never lays out an element outside its parent, and `uniform_list` does not
/// even build the rows it has scrolled past, so the laid-out path can never
/// produce one. A custom canvas paints wherever it likes, which is exactly why
/// `agent_paint_node` does the content-mask clip itself.
fn painted_canvas() -> impl IntoElement {
    div().w(px(100.)).h(px(40.)).overflow_hidden().child(
        canvas(
            |bounds, window, cx| {
                let card = |x: f32| Bounds {
                    origin: point(bounds.origin.x + px(x), bounds.origin.y),
                    size: size(px(40.), px(20.)),
                };
                agent_paint_node(Role::Card, "Painted", card(0.), window, cx);
                // Starts 200px right of a 100px-wide box: entirely outside.
                agent_paint_node(Role::Card, "PaintedOffscreen", card(200.), window, cx);
            },
            |_, _, _, _| {},
        )
        .size_full(),
    )
}

// -- fixtures -----------------------------------------------------------------

fn harness_with(seed: u64, timeout: Duration, stall: Duration) -> Harness {
    let root: gpui_agent::RootFactory = Arc::new(move |_: &mut Window, cx: &mut App| -> AnyView {
        cx.bind_keys([KeyBinding::new("ctrl-b", Bump, Some("Counter"))]);
        cx.new(|cx| Counter::new(stall, cx)).into()
    });
    Harness::headless(
        Config {
            mode: Mode::Headless,
            seed,
            call_timeout: timeout,
            ..Config::default()
        },
        root,
    )
    .expect("failed to start the harness")
}

fn harness() -> Harness {
    harness_with(7, Duration::from_secs(10), Duration::ZERO)
}

fn run(harness: &mut Harness, code: &str) -> Value {
    let result = harness.exec(code, Duration::from_secs(10));
    assert_eq!(result.error, None, "script failed:\n{code}");
    result.result
}

/// Reads the counter out of a snapshot without depending on where it sits.
const COUNT_LABEL: &str = r#"app.snapshot().find((node) => node.label.startsWith("count")).label"#;

/// The script the done-when list asks for: snapshot, find a button by label,
/// click it, snapshot again, and report whether anything changed.
const CLICK_SCRIPT: &str = r#"
    const before = app.snapshot();
    const button = before.find({ role: "button", label: "Increment" });
    app.click(button);
    const after = app.snapshot();
    ({
        clicked: button.label,
        changed: JSON.stringify(before.nodes) !== JSON.stringify(after.nodes),
        count: after.find((node) => node.label.startsWith("count")).label,
    })
"#;

// -- tests --------------------------------------------------------------------

#[test]
fn a_snapshot_names_every_instrumented_control() {
    let mut harness = harness();
    let shot = run(&mut harness, "app.snapshot()");
    let labels: Vec<&str> = shot["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        [
            "Source",
            "Target",
            "dropped 0",
            "Increment",
            "Modal",
            "Locked",
            "row 0",
            "row 1",
            "row 2",
            "row 3",
            "Stall",
            "count 0",
            "unanswered",
            "Wheel",
            "wheel 0,0 pan",
            "Painted",
            "PaintedOffscreen",
        ]
    );
    assert!(shot["frame"].as_u64().unwrap() > 0);
}

#[test]
fn clicking_a_button_found_by_label_changes_the_next_snapshot() {
    let mut harness = harness();
    let result = run(&mut harness, CLICK_SCRIPT);
    assert_eq!(result["clicked"], "Increment");
    assert_eq!(result["changed"], true);
    assert_eq!(result["count"], "count 1");
}

#[test]
fn the_same_seed_replays_byte_for_byte() {
    let once = {
        let mut harness = harness_with(31, Duration::from_secs(10), Duration::ZERO);
        serde_json::to_vec(&harness.exec(CLICK_SCRIPT, Duration::from_secs(10))).unwrap()
    };
    let twice = {
        let mut harness = harness_with(31, Duration::from_secs(10), Duration::ZERO);
        serde_json::to_vec(&harness.exec(CLICK_SCRIPT, Duration::from_secs(10))).unwrap()
    };
    assert_eq!(once, twice);
}

#[test]
fn acting_on_a_stale_frame_errors_instead_of_clicking() {
    let mut harness = harness();
    let result = harness.exec(
        r#"
            const stale = app.snapshot();
            const button = stale.find({ label: "Increment" });
            app.click(button);          // redraws, so `stale` is now a frame behind
            app.click(button);          // and this must not land anywhere
        "#,
        Duration::from_secs(10),
    );
    let error = result.error.expect("the second click should have failed");
    assert!(
        error.contains("StaleFrame: snapshot was"),
        "unexpected error: {error}"
    );

    // And nothing landed: one click, one increment.
    let after = run(&mut harness, COUNT_LABEL);
    assert_eq!(after, "count 1");
}

#[test]
fn restale_match_reacquires_the_node_by_role_and_label() {
    let mut harness = harness();
    let result = run(
        &mut harness,
        r#"
            const stale = app.snapshot();
            const button = stale.find({ label: "Increment" });
            app.click(button);
            app.click(button, { restale: "match" });
            app.snapshot().find((n) => n.label.startsWith("count")).label
        "#,
    );
    assert_eq!(result, "count 2");
}

#[test]
fn a_node_reference_that_does_not_match_its_frame_is_refused() {
    let mut harness = harness();
    let result = harness.exec(
        r#"
            const shot = app.snapshot();
            app.click({ ...shot.nodes[0], label: "Something Else" });
        "#,
        Duration::from_secs(10),
    );
    let error = result.error.expect("a forged node should have failed");
    assert!(error.contains("NoSuchNode"), "unexpected error: {error}");
}

/// The point of walking the pointer rather than teleporting it: gpui only
/// promotes a press into a drag once the pointer has moved far enough, and
/// only materializes `active_drag` on the repaint after that. A drag that
/// jumped straight to the target would drop nothing.
#[test]
fn a_drag_walks_the_pointer_and_lands_on_the_target() {
    let mut harness = harness();
    let result = run(
        &mut harness,
        r#"
            const shot = app.snapshot();
            app.drag(shot.find({ label: "Source" }), shot.find({ label: "Target" }));
            app.snapshot().find((n) => n.label.startsWith("dropped")).label
        "#,
    );
    assert_eq!(result, "dropped 1");
}

/// The other target shape. A canvas move has no destination control, so the
/// delta has to walk the pointer exactly as far as it was told — the drop here
/// only lands because `Target` is that far below `Source`.
#[test]
fn a_drag_can_name_its_destination_as_a_displacement() {
    let mut harness = harness();
    let result = run(
        &mut harness,
        r#"
            const shot = app.snapshot();
            const source = shot.find({ label: "Source" });
            const target = shot.find({ label: "Target" });
            app.drag(source, { dx: 0, dy: target.bounds.y - source.bounds.y });
            app.snapshot().find((n) => n.label.startsWith("dropped")).label
        "#,
    );
    assert_eq!(result, "dropped 1");
}

/// A pointer walked off the window is dropped by gpui, so the drag would end
/// short with nothing to say it had. Refusing is the only outcome a script can
/// act on.
#[test]
fn a_displacement_that_leaves_the_window_is_refused() {
    let mut harness = harness();
    let result = harness.exec(
        r#"app.drag(app.snapshot().find({ label: "Source" }), { dx: 0, dy: -1000 })"#,
        Duration::from_secs(10),
    );
    let error = result.error.expect("an off-window drag should have failed");
    assert!(error.contains("BadCall"), "unexpected error: {error}");
    assert!(
        error.contains("outside the"),
        "the error should say what it refused: {error}"
    );
}

#[test]
fn an_action_can_be_dispatched_by_name_and_by_keystroke() {
    let mut harness = harness();
    assert_eq!(
        run(
            &mut harness,
            r#"
                app.action("gpui_agent_test::Bump");
                app.key("ctrl-b");
                app.snapshot().find((n) => n.label.startsWith("count")).label
            "#,
        ),
        "count 2"
    );
}

#[test]
fn an_unparseable_keystroke_is_rejected_rather_than_panicking() {
    let mut harness = harness();
    let result = harness.exec(
        r#"app.key("definitely-not-a-key")"#,
        Duration::from_secs(10),
    );
    let error = result.error.expect("a bad keystroke should fail");
    assert!(error.contains("BadCall"), "unexpected error: {error}");
    // And the pump survived it.
    assert_eq!(run(&mut harness, "app.snapshot().nodes.length > 0"), true);
}

/// A silently-ignored option is worse than a broken one: the script would
/// carry on believing it had asked for something it never asked for.
#[test]
fn a_misspelt_option_is_refused() {
    let mut harness = harness();
    let result = harness.exec(
        r#"app.click(app.snapshot().find({ label: "Increment" }), { restail: "match" })"#,
        Duration::from_secs(10),
    );
    let error = result.error.expect("a misspelt option should have failed");
    assert!(error.contains("unknown option `restail`"), "{error}");

    // The spelling that exists still works, and so does the wire translation
    // for the one member whose option is not spelled the same on both sides.
    assert_eq!(
        run(&mut harness, "app.frames(2, { waitMs: 0 }).frame > 0"),
        true
    );
}

/// The empty-bounds contract, end to end: a painted control outside its mask
/// is registered — a script can see it exists — but has no box, and acting on
/// it is refused rather than clicking through to whatever is really there.
#[test]
fn a_painted_control_outside_its_mask_is_seen_but_not_clickable() {
    let mut harness = harness();

    // Compared as numbers, not as JSON: a whole float makes the round trip
    // through JavaScript as an integer, so `40.0` comes back `40`.
    let area = |harness: &mut Harness, label: &str| -> (f64, f64) {
        let box_ = run(
            harness,
            // Scoped, because `globalThis` persists between `exec` calls and a
            // bare `const` would be a redeclaration the second time round.
            &format!(
                r#"(() => {{
                    const n = app.snapshot().find({{ label: "{label}" }});
                    return [n.bounds.width, n.bounds.height];
                }})()"#
            ),
        );
        (
            box_[0].as_f64().expect("width"),
            box_[1].as_f64().expect("height"),
        )
    };
    assert_eq!(area(&mut harness, "Painted"), (40., 20.));
    // Zero along the axis it left, not `0 × 0`: `Bounds::intersect` clamps
    // each corner componentwise, so a card off to the right keeps its height.
    // What the harness actually leans on is `is_empty`, i.e. either dimension
    // being zero — assert that, not a particular shape.
    let (width, height) = area(&mut harness, "PaintedOffscreen");
    assert_eq!(width, 0., "off to the right, so the width should collapse");
    assert!(
        width == 0. || height == 0.,
        "the off-mask card should be empty in at least one axis, got {width}x{height}"
    );

    let result = harness.exec(
        r#"app.click(app.snapshot().find({ label: "PaintedOffscreen" }))"#,
        Duration::from_secs(10),
    );
    let error = result.error.expect("clicking a clipped card should fail");
    assert!(error.contains("NotVisible"), "unexpected error: {error}");

    // The one inside the mask is clickable, so the guard is not just refusing
    // everything painted.
    run(
        &mut harness,
        r#"app.click(app.snapshot().find({ label: "Painted" }))"#,
    );
}

/// Timing exists for a perf loop, so the properties that matter are: every
/// settling command is measured (not just `frames`), the numbers line up with
/// the frames a script can see, and reading is free.
#[test]
fn every_settled_frame_is_timed_and_readable() {
    let mut harness = harness();

    let before = run(&mut harness, "app.timings().frames.length")
        .as_u64()
        .unwrap();

    // A drag is the shape a scrub has: one settle per pointer step. If only
    // `frames` were instrumented this would add nothing, which is the failure
    // this test exists to catch.
    let after = run(
        &mut harness,
        r#"
            app.drag(app.snapshot().find({ label: "Source" }), { dx: 0, dy: 4 }, { steps: 6 });
            app.timings().frames.length
        "#,
    )
    .as_u64()
    .unwrap();
    // mouse-down, six moves, mouse-up, plus the two snapshots either side.
    assert!(
        after >= before + 8,
        "a six-step drag should have timed at least eight frames, got {}",
        after - before
    );

    let report = run(
        &mut harness,
        r#"
            const t = app.timings();
            const last = t.frames[t.frames.length - 1];
            ({
                mode: t.mode,
                keys: Object.keys(last).sort(),
                nonNegative: t.frames.every((f) => f.parkedMs >= 0 && f.drawMs >= 0),
                ordered: t.frames.every((f, i) => i === 0 || f.frame >= t.frames[i - 1].frame),
                current: last.frame,
            })
        "#,
    );
    assert_eq!(report["mode"], "headless");
    assert_eq!(
        report["keys"],
        serde_json::json!(["drawMs", "frame", "parkedMs"])
    );
    assert_eq!(report["nonNegative"], true);
    assert_eq!(report["ordered"], true);

    // The frame numbers are the same ones a snapshot reports, which is what
    // makes "filter to the frames my drag produced" possible at all.
    let snapshot_frame = run(&mut harness, "app.snapshot().frame").as_u64().unwrap();
    let timed = run(
        &mut harness,
        &format!("app.timings().frames.some((f) => f.frame === {snapshot_frame})"),
    );
    assert_eq!(
        timed, true,
        "frame {snapshot_frame} was drawn but not timed"
    );

    // Reading is free: it neither settles nor clears.
    let twice = run(
        &mut harness,
        "const a = app.timings(); JSON.stringify(a) === JSON.stringify(app.timings())",
    );
    assert_eq!(twice, true, "reading timings changed them");
}

/// The history is bounded, or a long profiling session grows without limit.
#[test]
fn the_timing_history_is_capped() {
    let mut harness = harness();
    let count = run(
        &mut harness,
        "app.frames(600, { waitMs: 0 }); app.timings().frames.length",
    )
    .as_u64()
    .unwrap();
    assert_eq!(count, 512);
}

/// A wheel handler is reachable no other way: it is scoped to a canvas hitbox
/// and answers only `ScrollWheelEvent`, so no amount of clicking or dragging
/// gets at it. This is the whole reason `app.scroll` exists.
#[test]
fn the_wheel_reaches_a_handler_that_clicks_cannot() {
    let mut harness = harness();
    let wheel = |harness: &mut Harness| {
        run(
            harness,
            r#"app.snapshot().find((n) => n.label.startsWith("wheel")).label"#,
        )
    };
    assert_eq!(wheel(&mut harness), "wheel 0,0 pan");

    // Clicking the same canvas does nothing, so the wheel is doing the work.
    run(
        &mut harness,
        r#"app.click(app.snapshot().find({ label: "Wheel" }))"#,
    );
    assert_eq!(wheel(&mut harness), "wheel 0,0 pan");

    run(
        &mut harness,
        r#"app.scroll(app.snapshot().find({ label: "Wheel" }), { dx: 30, dy: 60 })"#,
    );
    assert_eq!(wheel(&mut harness), "wheel 30,60 pan");
}

/// The modifier is what a timeline branches on to choose zoom over pan, so it
/// has to survive the trip as more than "some modifier was held".
#[test]
fn a_scroll_can_hold_a_modifier() {
    let mut harness = harness();
    run(
        &mut harness,
        r#"app.scroll(app.snapshot().find({ label: "Wheel" }), { dy: 10, modifiers: ["secondary"] })"#,
    );
    assert_eq!(
        run(
            &mut harness,
            r#"app.snapshot().find((n) => n.label.startsWith("wheel")).label"#
        ),
        "wheel 0,10 zoom"
    );

    let result = harness.exec(
        r#"app.scroll(app.snapshot().find({ label: "Wheel" }), { dy: 1, modifiers: ["hyper"] })"#,
        Duration::from_secs(10),
    );
    let error = result.error.expect("an unknown modifier should fail");
    assert!(error.contains("unknown modifier"), "unexpected: {error}");
}

/// Steps are the point of a scripted wheel: the delta is split across them and
/// each gets its own settled frame, so a handler that accumulates sees the
/// same shape a real wheel produces — and each step is timed.
#[test]
fn a_scroll_walks_in_steps_and_each_one_is_timed() {
    let mut harness = harness();
    let before = run(&mut harness, "app.timings().frames.length")
        .as_u64()
        .unwrap();
    // Addressed by point, not by node — but the point is taken from the node,
    // so this also proves the two ways of naming a spot agree.
    run(
        &mut harness,
        r#"
            const b = app.snapshot().find({ label: "Wheel" }).bounds;
            app.scroll({ x: b.x + b.width / 2, y: b.y + b.height / 2 }, { dy: 100, steps: 10 });
        "#,
    );
    let after = run(&mut harness, "app.timings().frames.length")
        .as_u64()
        .unwrap();
    assert!(
        after >= before + 10,
        "ten steps should have timed ten frames, got {}",
        after - before
    );

    // Scrolling at a bare point reaches the same handler as scrolling a node,
    // which is what makes a canvas with no control at the spot addressable.
    assert_eq!(
        run(
            &mut harness,
            r#"app.snapshot().find((n) => n.label.startsWith("wheel")).label"#
        ),
        "wheel 0,100 pan"
    );
}

#[test]
fn a_disabled_control_says_so() {
    let mut harness = harness();
    let result = run(
        &mut harness,
        r#"app.snapshot().findAll({ role: "button" }).map((n) => [n.label, n.enabled])"#,
    );
    assert_eq!(
        result,
        serde_json::json!([
            ["Increment", true],
            ["Modal", true],
            ["Locked", false],
            ["Stall", true],
        ])
    );
}

#[test]
fn a_virtualized_list_names_only_the_rows_it_built() {
    let mut harness = harness();
    let rows: Vec<String> = serde_json::from_value(run(
        &mut harness,
        r#"app.snapshot().findAll({ role: "row" }).map((n) => n.label)"#,
    ))
    .unwrap();
    assert!(!rows.is_empty(), "the list built no rows at all");
    assert!(
        rows.len() < 200,
        "the list built all 200 rows: {}",
        rows.len()
    );
    assert_eq!(rows[0], "row 0");

    // Every named row is somewhere you could actually click.
    let clickable = run(
        &mut harness,
        r#"app.snapshot().findAll({ role: "row" }).every((n) => n.bounds.height > 0)"#,
    );
    assert_eq!(clickable, true);
}

#[test]
fn an_app_modal_prompt_does_not_deadlock() {
    let mut harness = harness();
    // The click returns, the app keeps answering, and the prompt is simply
    // outstanding — which is the shape a driver has to be able to observe.
    let result = run(
        &mut harness,
        r#"
            app.click(app.snapshot().find({ label: "Modal" }));
            app.frames(2);
            app.snapshot().findAll({ role: "text" }).map((n) => n.label)
        "#,
    );
    assert_eq!(result[2], "unanswered");

    let after = run(
        &mut harness,
        r#"app.snapshot().find({ label: "Increment" }) !== undefined"#,
    );
    assert_eq!(after, true);
}

#[test]
fn a_call_the_app_cannot_answer_in_time_fails_cleanly() {
    let mut harness = harness_with(7, Duration::from_millis(200), Duration::from_millis(1500));
    let result = harness.exec(
        r#"app.click(app.snapshot().find({ label: "Stall" }))"#,
        Duration::from_secs(10),
    );
    let error = result
        .error
        .expect("the stalled click should have timed out");
    assert!(error.contains("Timeout"), "unexpected error: {error}");
}

#[test]
fn a_runaway_script_is_interrupted() {
    let mut harness = harness();
    let result = harness.exec("while (true) {}", Duration::from_millis(250));
    let error = result
        .error
        .expect("an infinite loop should have been cut off");
    assert!(error.contains("Timeout"), "unexpected error: {error}");

    // The interpreter is still usable afterwards.
    assert_eq!(run(&mut harness, "1 + 1"), 2);
}

#[test]
fn globals_persist_across_exec_and_reset_clears_them() {
    let mut harness = harness();
    run(&mut harness, "globalThis.remembered = 41");
    assert_eq!(run(&mut harness, "remembered + 1"), 42);

    harness.reset().expect("reset failed");
    let result = harness.exec("typeof remembered", Duration::from_secs(10));
    assert_eq!(result.error, None);
    assert_eq!(result.result, "undefined");
}

#[test]
fn reset_rebuilds_the_app() {
    let mut harness = harness();
    run(
        &mut harness,
        r#"app.click(app.snapshot().find({ label: "Increment" }))"#,
    );
    harness.reset().expect("reset failed");
    assert_eq!(run(&mut harness, COUNT_LABEL), "count 0");
}

#[test]
fn console_output_is_captured_and_truncated() {
    let mut harness = harness();
    let small = harness.exec("console.log('hello', {a: 1}); 0", Duration::from_secs(10));
    assert_eq!(small.stdout, r#"hello {"a":1}"#);

    let big = harness.exec(
        "for (let i = 0; i < 5000; i++) console.log('line ' + i + ' ' + 'x'.repeat(40)); 0",
        Duration::from_secs(30),
    );
    assert!(
        big.stdout.len() < 9 * 1024,
        "stdout was {}",
        big.stdout.len()
    );
    assert!(big.stdout.contains("lines elided]"), "{}", big.stdout);
    assert!(big.stdout.starts_with("line 0 "));
    assert!(big.stdout.trim_end().ends_with('x'));
}

#[test]
fn a_screenshot_in_headless_mode_says_why_it_cannot() {
    let mut harness = harness();
    let result = harness.exec("app.screenshot()", Duration::from_secs(10));
    let error = result.error.expect("headless mode cannot screenshot");
    assert!(error.contains("Unsupported"), "unexpected error: {error}");
}

#[test]
fn an_unknown_action_names_itself_in_the_error() {
    let mut harness = harness();
    let result = harness.exec(
        "app.action('luma::NotARealAction')",
        Duration::from_secs(10),
    );
    let error = result.error.expect("an unregistered action should fail");
    assert!(error.contains("BadAction"), "unexpected error: {error}");
}

/// `app.help()` is the only documentation a driver gets, so a member that
/// exists on one side and not the other is a bug in the contract, not a
/// docs nit.
#[test]
fn help_matches_the_bound_surface() {
    let mut harness = harness();
    let bound = run(&mut harness, "Object.keys(app).sort()");
    let bound: Vec<String> = serde_json::from_value(bound).unwrap();

    let declared = run(
        &mut harness,
        r#"
            const dts = app.help();
            const body = dts.slice(dts.indexOf("interface App {") + "interface App {".length);
            const members = new Set();
            for (const match of body.matchAll(/^  ([a-zA-Z]+)[(<]/gm)) members.add(match[1]);
            [...members].sort()
        "#,
    );
    let declared: Vec<String> = serde_json::from_value(declared).unwrap();

    assert_eq!(bound, declared);
    assert!(!bound.is_empty(), "the .d.ts scrape found nothing");
    // And `help` really is the file, not a summary of it.
    assert_eq!(
        run(&mut harness, "app.help()"),
        Value::String(gpui_agent::API_DTS.to_string())
    );
}
