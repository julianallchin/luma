//! The stage pane and the chrome floating over it: toolbar, fullscreen
//! button, selection card, marquee, pivot dot and audition badge.
use super::*;

pub(crate) enum Chrome {
    Embedded { venue_tools: Option<AnyElement> },
    Fullscreen { transport: Option<AnyElement> },
}

/// The stage pane with floating controls.
///
/// # Calling this is what starts the redraw loop
///
/// The request below is unconditional and self-sustaining: asking at the top of
/// a render is what makes the next one happen. There is deliberately no
/// "visible" flag guarding it, because a flag would be a second answer to a
/// question the element tree already answers — the loop stops when, and only
/// when, the shell stops mounting this pane. Every caller must therefore treat
/// *not calling* as the off switch (see [`Luma::sync_visualizer`] and
/// `shell::workspace_body`); a hidden pane that still rendered would keep a GPU
/// busy drawing frames nobody sees.
///
/// Callers render this inside a live `shell::Region` of its own. The request
/// notifies the view being rendered, so it asks for a frame of the stage's
/// region, not of the app: the cached panes around the stage are not rebuilt.
pub(crate) fn visualizer(
    state: &mut Visualizer,
    app: &Entity<Luma>,
    library: &Library,
    window: &mut Window,
    chrome: Chrome,
    focus: &gpui::FocusHandle,
) -> impl IntoElement {
    // Continuous redraw: asking at the top of a render is what makes the next
    // one happen, and CVDisplayLink paces it (spec §4.3). An export draws no
    // stage, so it asks for nothing.
    if !state.exporting {
        window.request_animation_frame();
    }
    // Stamped here so the stage's prepaint can say how much of the frame the
    // rest of the UI thread spent before reaching it, and counted so a gap with
    // *no* render can be told from a gap full of other work.
    {
        let mut stage = state.stage.borrow_mut();
        stage.requested_at = Some(Instant::now());
        stage.renders_since_prepaint = stage.renders_since_prepaint.saturating_add(1);
    }
    state.presentation = matches!(&chrome, Chrome::Fullscreen { .. });
    state.sync_pointer_lock(window);
    state.fly_tick(Instant::now(), window.modifiers().shift);
    let (venue_tools, transport) = match chrome {
        Chrome::Embedded { venue_tools } => (venue_tools, None),
        Chrome::Fullscreen { transport } => (None, transport),
    };
    let venue = venue_tools.is_some();
    let body = body(state, app, library);
    let (floating, bar) = overlay_toolbar(state, app, venue_tools, transport);
    state.bottom_bar_visible = bar;
    // The stage renders every frame, so nothing else has to ask for the
    // frames the popovers' exits play over.
    state.fps_panel.tick_close(state.reduced_motion);
    state.settings.tick_close(state.reduced_motion);
    let fps = fps_overlay(state, app);
    let pane = state.stage.borrow().pane;
    let builder = state
        .build
        .as_ref()
        .filter(|_| !state.presentation)
        .map(|build| {
            crate::stage::build_layer(
                build,
                &state.camera,
                pane.origin,
                (f32::from(pane.size.width), f32::from(pane.size.height)),
                // With no GPU there is no canvas and no window listener, so the
                // claim card is also the pointer's way in — the headless path.
                state.gpu_enabled,
                app,
            )
        });
    let measure = {
        let stage = Rc::clone(&state.stage);
        canvas(
            move |bounds, _, _| {
                stage.borrow_mut().pane = bounds;
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0()
    };
    // The shift-drag sweep, drawn while it is live so the operator sees what
    // the release will select.
    let marquee = state
        .marquee_rect()
        .filter(|_| !state.presentation)
        .map(|rect| {
            let size = rect.size();
            div()
                .absolute()
                .left(px(rect.min.x))
                .top(px(rect.min.y))
                .w(px(size.x))
                .h(px(size.y))
                .border_1()
                .border_color(ladder::primary())
                .bg(gpui::Rgba {
                    a: 0.12,
                    ..ladder::primary()
                })
        });
    // The pivot of the last orbit or wheel step, fading out.
    let pivot_dot = state.pivot_dot_at(Instant::now()).map(|(at, fade)| {
        let radius = PIVOT_DOT_SIZE / 2.;
        div()
            .absolute()
            .left(px(at.x) - radius)
            .top(px(at.y) - radius)
            .size(PIVOT_DOT_SIZE)
            .rounded_full()
            .bg(luma_ui::glass::ink(0.9 * fade))
            .border_1()
            .border_color(luma_ui::glass::scrim(0.5 * fade))
    });
    let mut keys = gpui::KeyContext::default();
    keys.add(crate::keymap::context::VISUALIZER);
    // A fly takes W, A, S, D, Q and E, so the stage's own letters — W and E
    // among them — are off while the right button is held.
    if venue && state.fly.is_none() {
        keys.add(crate::keymap::context::STAGE);
        keys.add(crate::keymap::context::PATCH);
    }
    let fly_key = |down: bool| {
        let app = app.clone();
        move |key: &str, cx: &mut gpui::App| {
            app.update(cx, |this, cx| {
                // No notify: see `Visualizer::input_redraws`.
                if this
                    .visualizer_mut()
                    .is_some_and(|state| state.fly_key(key, down))
                {
                    cx.stop_propagation();
                }
            });
        }
    };
    let (pressed, released) = (fly_key(true), fly_key(false));
    div()
        .size_full()
        .key_context(keys)
        .track_focus(focus)
        .on_key_down(move |event: &gpui::KeyDownEvent, _, cx| {
            pressed(&event.keystroke.key, cx);
        })
        .on_key_up(move |event: &gpui::KeyUpEvent, _, cx| {
            released(&event.keystroke.key, cx);
        })
        .flex()
        .flex_col()
        .bg(ladder::background())
        .child(
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(body)
                .child(measure)
                .children(marquee)
                .children(pivot_dot)
                .children(builder)
                .children(fps)
                .children(state.stage.borrow().exports.notice())
                .children(floating)
                .child(fullscreen_button(state.presentation, app))
                .when(matches!(state.status, Status::Live), |d| {
                    d.child(settings::trigger(state, app))
                }),
        )
        .children(state.audition.as_ref().map(audition_badge))
        .agent_node(
            Role::Card,
            state.lit.as_ref().map_or_else(
                || state.venue_name.clone(),
                |lit| format!("RIG SCORE #{}", lit.ordinal),
            ),
        )
}

/// A persistent exit stays available even when the scene is loading or failed.
fn fullscreen_button(fullscreen: bool, app: &Entity<Luma>) -> impl IntoElement {
    let label = if fullscreen {
        "Exit fullscreen"
    } else {
        "Fullscreen visualizer"
    };
    let icon = if fullscreen {
        luma_ui::icons::IconName::Minimize
    } else {
        luma_ui::icons::IconName::Expand
    };
    let app = app.clone();
    div()
        .absolute()
        .top(CORNER_INSET)
        .right(CORNER_INSET)
        .child(
            luma_ui::button("", luma_ui::Enabled::Yes)
                .id("visualizer-fullscreen")
                .size(CORNER_CONTROL)
                .p_0()
                .occlude()
                .child(gpui_component::Icon::new(icon).size(px(18.)))
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                })
                .on_click(move |_, window, cx| {
                    app.update(cx, |this, cx| this.toggle_visualizer_fullscreen(window, cx))
                })
                .agent_node(Role::Button, label),
        )
}

/// Only mount the floating surface when it has controls to display.
fn overlay_toolbar(
    state: &Visualizer,
    app: &Entity<Luma>,
    venue_tools: Option<AnyElement>,
    transport: Option<AnyElement>,
) -> (Option<AnyElement>, bool) {
    let gizmo = !state.presentation
        && matches!(state.status, Status::Live)
        && state.selection_gizmo_space() != luma_scene::gizmo::GizmoSpace::DISABLED;
    // The builder's dialogs float over the picture whether or not there is a
    // bar under them; the bar is only a transport or a gizmo's two modes.
    let venue_tools = venue_tools.filter(|_| matches!(state.status, Status::Live));
    if transport.is_none() && !gizmo {
        return (venue_tools, false);
    }
    let current = state.gizmo_mode;
    let mode = |label: &'static str, mode: GizmoMode| {
        let app = app.clone();
        luma_ui::float::segment(label, current == mode, label)
            .id(label)
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| {
                    if let Some(state) = this.visualizer_mut() {
                        state.gizmo_mode = mode;
                    }
                    cx.notify();
                });
            })
            .agent_node(Role::Toggle, label)
    };
    let bar = div()
        .absolute()
        .bottom(TOOLBAR_OVERLAY_BOTTOM)
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            luma_ui::float::popover_card()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .p(px(4.))
                // The bar, not the centring row it sits in: an opaque
                // surface over the viewport owns the pointer it covers
                // (see [`listen`]), and the row is air either side of it.
                .occlude()
                .children(transport)
                // The two gizmo modes are one choice, so they share one
                // track. Zoom is the wheel's (and `=`/`-`), not a button's
                // — a camera verb with a pointer gesture needs no chrome.
                //
                // The mode switch uses the same joint freedoms as the
                // handles, including mixed selections with no shared frame.
                .when(gizmo, |bar| {
                    bar.child(
                        luma_ui::float::segmented()
                            .child(mode("Translate", GizmoMode::Translate))
                            .child(mode("Rotate", GizmoMode::Rotate)),
                    )
                })
                .map(luma_ui::float::frosted_card),
        )
        .children(venue_tools)
        .agent_node(Role::Card, "Visualizer toolbar")
        .into_any_element();
    (Some(bar), true)
}

/// What says the stage is playing a hovered preset, not the score: the
/// preset's name on the toolbar's surface, centred on the stage's top edge,
/// on the corner controls' line.
fn audition_badge(audition: &crate::track_editor::Audition) -> AnyElement {
    let label: gpui::SharedString = format!("Preview · {}", audition.name).into();
    div()
        .absolute()
        .top(CORNER_INSET)
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            luma_ui::float::popover_card()
                .px(px(10.))
                .py(px(4.))
                .text_size(px(12.))
                .text_color(ladder::foreground())
                .child(label.clone())
                .map(luma_ui::float::frosted_card),
        )
        .agent_node(Role::Text, label)
        .into_any_element()
}
