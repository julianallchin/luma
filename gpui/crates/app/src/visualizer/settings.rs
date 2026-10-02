//! Compact environment dock shared by every visualizer.
use super::*;
use luma_ui::node::AgentNode as _;
use luma_ui::{float, glass};

/// Persistent clocks survive render calls.
#[derive(Default)]
pub(super) struct DockMotion {
    tooltip: Transition,
    pub(super) switches: [Transition; 8],
    knob_hovered: bool,
    light_dragging: bool,
    knob_bounds: Rc<std::cell::Cell<gpui::Bounds<Pixels>>>,
}

impl DockMotion {
    pub(super) fn new(reduced: bool) -> Self {
        let transition = || Transition {
            reduced,
            ..Transition::default()
        };
        Self {
            tooltip: transition(),
            switches: std::array::from_fn(|_| transition()),
            knob_hovered: false,
            light_dragging: false,
            knob_bounds: Rc::default(),
        }
    }
}

pub(super) struct Transition {
    from: f32,
    target: f32,
    since: Instant,
    reduced: bool,
}
impl Default for Transition {
    fn default() -> Self {
        Self {
            from: 0.,
            target: 0.,
            since: Instant::now(),
            reduced: false,
        }
    }
}
impl DockMotion {
    /// Whether a switch is still sliding to its new side.
    fn switching(&self) -> bool {
        let now = Instant::now();
        self.switches
            .iter()
            .any(|switch| switch.value(now) != switch.target)
    }
}

impl Transition {
    fn value(&self, now: Instant) -> f32 {
        if self.reduced {
            return self.target;
        }
        let millis = if self.target > self.from {
            luma_ui::motion::QUICK
        } else {
            luma_ui::motion::SNAP
        };
        let duration = millis as f32 / 1000. * luma_ui::motion::speed_scale();
        let t = (now.duration_since(self.since).as_secs_f32() / duration).clamp(0., 1.);
        self.from + (self.target - self.from) * luma_ui::motion::ROOT.eval(t)
    }
    pub(super) fn sample(&mut self, target: bool) -> f32 {
        let now = Instant::now();
        let target = if target { 1. } else { 0. };
        if self.target != target {
            self.from = self.value(now);
            self.target = target;
            self.since = now;
        }
        self.value(now)
    }
}

pub(super) fn trigger(state: &Visualizer, app: &Entity<Luma>) -> AnyElement {
    let toggle = app.clone();
    let close = app.clone();
    let mut content = float::popover_card()
        .w(px(268.))
        .p(px(14.))
        .gap(px(12.))
        .child(float::label("View settings"))
        .child(super::view_controls(state, app))
        .child(float::divider())
        .child(section("Debug", debug_rows(state, app)));
    if let Some(error) = &state.view_setting_error {
        content = content.child(float::error_row(error.clone()));
    }
    let open = state.settings.is_open();
    let camera = luma_ui::icon_toggle(luma_ui::icons::IconName::Camera, open)
        .id("visualizer-settings")
        .on_click(move |_, _, cx| {
            toggle.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.settings.toggle();
                }
                cx.notify();
            });
        })
        .agent_node(Role::Toggle, "Render settings")
        .agent_focused(open);
    let dock = float::popover_card()
        .w(px(DOCK_WIDTH))
        .p(px(6.))
        .items_center()
        .gap(px(10.))
        .occlude()
        .child(camera)
        .child(light_slider(state, app));
    let content = content
        .agent_node(Role::Card, "Render settings")
        .into_any_element();
    div()
        .absolute()
        .right(px(16.))
        .bottom(TOOLBAR_OVERLAY_BOTTOM)
        .child(float::frosted_card(dock))
        .when(state.settings.is_shown(), |d| {
            d.child(match state.settings.exit() {
                Some(progress) => float::anchored_left_closing(
                    "view-settings-card",
                    DOCK_WIDTH,
                    content,
                    progress,
                ),
                // A press on the camera lands outside the card, so the
                // dismissal takes it and the toggle never runs: one press,
                // closed.
                None => float::anchored_left(
                    "view-settings-card",
                    DOCK_WIDTH,
                    float::Dismiss::on_press_out(move |_, cx| {
                        close.update(cx, |this, cx| {
                            if let Some(state) = this.visualizer_mut() {
                                state.settings.close();
                            }
                            cx.notify();
                        });
                    }),
                    content,
                ),
            })
        })
        .into_any_element()
}

/// The dock's width, which the settings card hangs to the left of.
const DOCK_WIDTH: f32 = 40.;

/// How wide the environment panel beside the stage is. The shell sizes the
/// panel's cached region with it, since a cached region is not measured.
pub(crate) const ENVIRONMENT_PANEL_WIDTH: f32 = 260.;

/// The room itself, beside the stage on the venue tab: indoor or outdoor, the sun and the
/// haze. These are venue truth, saved on the venue row, so they live with the
/// venue and not with the render settings, which are about the picture.
///
/// The panel is drawn in a cached region of its own, not with the stage, so the
/// stage's frames do not reach it: a switch that is still sliding asks for its
/// own next frame.
pub(crate) fn environment_panel(
    state: &Visualizer,
    app: &Entity<Luma>,
    window: &mut Window,
) -> AnyElement {
    let environment = state.venue_environment();
    let mut rows = div()
        .id("venue-environment-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(12.))
        .px(px(12.))
        .pb(px(12.))
        .child(environment_card(environment, app))
        .when_some(sun_rows(environment, app), |rows, sun| {
            rows.child(float::divider()).child(section("Sun", sun))
        })
        .when_some(cloud_rows(environment, app), |rows, clouds| {
            rows.child(float::divider())
                .child(section("Clouds", clouds))
        })
        .child(float::divider())
        .child(section("Floor", floor_rows(environment, app)))
        .child(float::divider())
        .child(section("Haze", super::haze_rows(state, app)));
    if state.settings_motion.borrow().switching() {
        window.request_animation_frame();
    }
    for error in [&state.environment_error, &state.haze_error]
        .into_iter()
        .flatten()
    {
        rows = rows.child(float::error_row(error.clone()));
    }
    div()
        .flex_none()
        .w(px(ENVIRONMENT_PANEL_WIDTH))
        .h_full()
        .overflow_hidden()
        .border_l_1()
        .border_color(ladder::trim())
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .px(px(12.))
                .py(px(8.))
                .text_size(px(12.5))
                .child("Environment"),
        )
        .child(rows)
        .agent_node(Role::Card, "Venue environment")
        .into_any_element()
}

/// The Debug section: the reflection probes, their debug balls, and the
/// camera export.
fn debug_rows(state: &Visualizer, app: &Entity<Luma>) -> AnyElement {
    let probes = state.render_controls.probes;
    // What the renderer is handed, read back from the settings it is sent.
    let sent = state.render_settings().probes;
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(super::view_toggle(
            state,
            app,
            "Reflection probes",
            probes.enabled,
            ViewToggle::ReflectionProbes,
        ))
        .child(super::view_toggle(
            state,
            app,
            "Show probes",
            probes.debug,
            ViewToggle::ShowProbes,
        ))
        .child(div().size_0().agent_node(
            Role::Text,
            format!(
                "Renderer probes = {}, balls {}",
                if sent.enabled { "on" } else { "off" },
                if sent.debug { "on" } else { "off" }
            ),
        ))
        .child(export_camera(app))
        .into_any_element()
}

/// Write the view's camera, sun and shadow cascades to a file. The same
/// action as the stage's Ctrl+Shift+E (Cmd on macOS).
fn export_camera(app: &Entity<Luma>) -> AnyElement {
    let app = app.clone();
    luma_ui::button("Export camera", luma_ui::Enabled::Yes)
        .id("export-camera")
        .on_click(move |_, _, cx| {
            app.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.export_camera();
                }
                cx.notify();
            });
        })
        .agent_node(Role::Button, "Export camera")
        .into_any_element()
}

#[derive(Clone)]
struct LightDrag;
impl Render for LightDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn light_slider(state: &Visualizer, app: &Entity<Luma>) -> AnyElement {
    let environment = state.venue_environment();
    let tooltip = {
        let mut motion = state.settings_motion.borrow_mut();
        let visible = motion.knob_hovered || motion.light_dragging;
        motion.tooltip.sample(visible)
    };
    let hover = app.clone();
    let drag = app.downgrade();
    let knob_bounds = state.settings_motion.borrow().knob_bounds.clone();
    let measured_knob = knob_bounds.clone();
    let indoor = matches!(environment, VenueEnvironment::Indoor { .. });
    let fraction = if indoor {
        environment.house_level()
    } else {
        (environment.sun_elevation_deg() + 90.) / 180.
    };
    let label = if indoor {
        "House lights"
    } else {
        "Time of day"
    };
    let value = if indoor {
        format!("{:.0}%", fraction * 100.)
    } else {
        // Elevation describes the rising half of a solar cycle: midnight,
        // horizon at 06:00, and zenith at noon. No location/calendar is stored.
        let minutes = (fraction * 720.).round() as u32;
        format!("{:02}:{:02}", minutes / 60, minutes % 60)
    };
    let app = app.clone();
    div()
        .id("visualizer-light-slider")
        .relative()
        .w(px(28.))
        .h(px(112.))
        .mb(px(4.))
        .cursor_ns_resize()
        .child(
            div()
                .absolute()
                .left(px(12.))
                .top(px(6.))
                .w(px(4.))
                .h(px(100.))
                .rounded_full()
                .bg(glass::wash(0.15)),
        )
        .child(
            div()
                .absolute()
                .left(px(12.))
                .bottom(px(6.))
                .w(px(4.))
                .h(px(100. * fraction))
                .rounded_full()
                .bg(ladder::foreground_alpha(0.6)),
        )
        .child(
            div()
                .id("light-knob")
                .on_hover(move |hovered, _, cx| {
                    hover.update(cx, |this, cx| {
                        if let Some(state) = this.visualizer_mut() {
                            state.settings_motion.borrow_mut().knob_hovered = *hovered;
                        }
                        cx.notify();
                    });
                })
                .absolute()
                .left(px(7.))
                .top(px(100. * (1. - fraction)))
                .size(px(14.))
                .rounded_full()
                .bg(ladder::foreground())
                .child(
                    canvas(
                        move |bounds, _, _| measured_knob.set(bounds),
                        |_, (), _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .when(tooltip > 0., |knob| {
                    let bubble = float::popover_card()
                        .px(px(8.))
                        .py(px(4.))
                        .text_size(px(11.))
                        .font_family(luma_ui::fonts::MONO)
                        .text_color(ladder::foreground_alpha(0.85))
                        .whitespace_nowrap()
                        .child(value.clone());
                    knob.child(
                        div()
                            .absolute()
                            .left(px(-10. + 3. * (1. - tooltip)))
                            .top(px(-5.))
                            .size_0()
                            .child(
                                gpui::deferred(
                                    gpui::anchored().anchor(gpui::Anchor::TopRight).child(
                                        div().opacity(tooltip).child(float::frosted_card(bubble)),
                                    ),
                                )
                                .priority(2),
                            ),
                    )
                }),
        )
        .on_drag(LightDrag, move |_, _, window, cx| {
            cx.stop_propagation();
            let _ = drag.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.settings_motion.borrow_mut().light_dragging = true;
                }
                cx.notify();
            });
            let drag = drag.clone();
            let knob_bounds = knob_bounds.clone();
            cx.new(|cx| {
                cx.on_release_in(window, move |_, window, cx| {
                    let _ = drag.update(cx, |this, cx| {
                        if let Some(state) = this.visualizer_mut() {
                            let mut motion = state.settings_motion.borrow_mut();
                            motion.light_dragging = false;
                            // Hover callbacks resume on the next frame after a drag.
                            motion.knob_hovered =
                                knob_bounds.get().contains(&window.mouse_position());
                        }
                        cx.notify();
                    });
                })
                .detach();
                LightDrag
            })
        })
        .on_drag_move(move |event: &gpui::DragMoveEvent<LightDrag>, _, cx| {
            let fraction = (1.
                - (f32::from(event.event.position.y - event.bounds.top()) - 6.) / 100.)
                .clamp(0., 1.);
            app.update(cx, |this, cx| {
                this.set_visualizer_environment(
                    if indoor {
                        VenueEnvironment::indoor(fraction)
                    } else {
                        // The dial moves the sun up and down, not round.
                        VenueEnvironment::outdoor(fraction * 180. - 90.)
                            .with_sun_azimuth(environment.sun_azimuth_deg())
                            .with_clouds(environment.clouds())
                    }
                    .with_floor(environment.floor()),
                    cx,
                )
            });
        })
        .child(
            div()
                .absolute()
                .size_0()
                .overflow_hidden()
                .agent_node(Role::Text, format!("{label} = {value}")),
        )
        .agent_node(Role::Slider, label)
        .into_any_element()
}

impl Luma {
    pub(crate) fn set_visualizer_environment(
        &mut self,
        environment: VenueEnvironment,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.set_venue_environment(environment);
        state.environment_edited = true;
        state.environment_error = None;
        *state.environment_pending.borrow_mut() = Some(environment);
        // One write in flight. Scrubbing updates the picture immediately and
        // coalesces later values so an older write cannot win over the last.
        if !state.environment_saving {
            self.save_visualizer_environment(cx);
        }
        cx.notify();
    }

    fn save_visualizer_environment(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.environment_saving = true;
        let venue = state.venue_id.clone();
        let queue = state.environment_pending.clone();
        cx.spawn(async move |this, cx| {
            // Keep draining even if navigation drops this visualizer. Its
            // final scrub value still belongs in the saved venue.
            loop {
                let Some(value) = queue.borrow_mut().take() else {
                    break;
                };
                let Ok(pending) = this.update(cx, |this, _| {
                    this.library.set_venue_environment(&venue, value)
                }) else {
                    break;
                };
                let result = pending.await;
                this.update(cx, |this, cx| {
                    let Some(state) = this
                        .visualizer_mut()
                        .filter(|s| Rc::ptr_eq(&s.environment_pending, &queue))
                    else {
                        return;
                    };
                    state.environment_saving = queue.borrow().is_some();
                    if !state.environment_saving {
                        state.environment_error = result
                            .err()
                            .map(|error| format!("Could not save render settings: {error}"));
                    }
                    cx.notify();
                })
                .ok();
                if queue.borrow().is_none() {
                    break;
                }
            }
        })
        .detach();
    }

    /// A view toggle: applied to the stage now, persisted where it is durable.
    ///
    /// The three tiers of this panel meet here. Haze is **venue truth** and goes
    /// on the venue row; grid and gizmos are **local device settings** and follow
    /// the operator between rooms; fixture shadows is a session dial and outlives
    /// nothing.
    pub(super) fn toggle_view_control(&mut self, control: ViewToggle, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.render_controls.toggle(control);
        let (grid, gizmos) = (
            state.render_controls.grid_enabled,
            state.render_controls.gizmos_enabled,
        );
        match control {
            ViewToggle::Haze => self.save_venue_haze(cx),
            // Straight through `set_setting`: it is already FIFO-ordered, and a
            // toggle cannot emit faster than a hand can click, so the
            // coalescing the scrubs need would buy nothing here.
            ViewToggle::Grid => self.write_view_setting("stage_grid", grid.to_string(), cx),
            ViewToggle::Gizmos => self.write_view_setting("stage_gizmos", gizmos.to_string(), cx),
            ViewToggle::AutoExposure | ViewToggle::Footage => self.save_look(cx),
            // Session dials: they outlive nothing.
            ViewToggle::FixtureShadows | ViewToggle::ReflectionProbes | ViewToggle::ShowProbes => {}
        }
        cx.notify();
    }

    /// A view scrub: applied to the stage now, persisted where it is durable.
    pub(super) fn set_view_value(
        &mut self,
        control: ViewValue,
        value: f32,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.render_controls.set(control, value);
        match control {
            ViewValue::RenderScale => {
                let percent = state.render_controls.render_scale_percent;
                self.save_view_setting("render_scale", percent.to_string(), cx);
            }
            control if control.is_look() => self.save_look(cx),
            _ => self.save_venue_haze(cx),
        }
        cx.notify();
    }

    /// Choose the tone curve: applied to the stage now, persisted with the
    /// rest of the look.
    pub(super) fn set_view_tone(&mut self, tone: scene_desc::ToneCurve, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.render_controls.look.tone = tone;
        self.save_look(cx);
        cx.notify();
    }

    /// Choose the glare style: applied now, persisted with the look.
    pub(super) fn set_view_glare_style(
        &mut self,
        style: scene_desc::GlareStyle,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.render_controls.look.glare.style = style;
        self.save_look(cx);
        cx.notify();
    }

    /// Persist the look the render controls now hold, as one setting.
    fn save_look(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        match serde_json::to_string(&state.render_controls.look) {
            Ok(json) => self.save_view_setting("stage_look", json, cx),
            Err(error) => {
                state.view_setting_error = Some(format!("Could not save stage_look: {error}"))
            }
        }
    }

    /// The renderer's cost level: applied to the stage now and kept as the
    /// `stage_low_quality` device setting, like the render percent.
    pub(super) fn set_view_quality(
        &mut self,
        quality: scene_desc::Quality,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        if state.render_controls.quality == quality {
            return;
        }
        state.render_controls.quality = quality;
        // A new level starts from its whole pixel budget.
        state.stage.borrow_mut().dynamic = super::DynamicBudget::default();
        let low = quality == scene_desc::Quality::Low;
        self.write_view_setting("stage_low_quality", low.to_string(), cx);
        cx.notify();
    }

    /// One local view setting, written once.
    ///
    /// Deliberately *not* `Luma::write_setting`: that one re-reads the whole
    /// settings record afterwards to repaint the Settings screen, which is a
    /// screen this panel is not. The stage already holds the new value, so the
    /// only thing left to report is a write that did not land.
    fn write_view_setting(&mut self, key: &'static str, value: String, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.view_setting_error = None;
        let pending = self.library.set_setting(key, &value);
        cx.spawn(async move |this, cx| {
            let Err(error) = pending.await else {
                return;
            };
            this.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.view_setting_error = Some(format!("Could not save {key}: {error}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Persist the haze the render controls now hold.
    ///
    /// The render controls are the authority — [`RenderControls::toggle`] and
    /// [`RenderControls::set`] have already sanitized it — so this takes no value. Same guard as
    /// [`Self::save_visualizer_environment`], for the same reason: a dragged
    /// density emits a value per pointer move.
    fn save_venue_haze(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        let haze = state.render_controls.haze;
        state.haze_edited = true;
        state.haze_error = None;
        *state.haze_pending.borrow_mut() = Some(haze);
        if !state.haze_saving {
            self.drain_venue_haze(cx);
        }
    }

    fn drain_venue_haze(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.haze_saving = true;
        let venue = state.venue_id.clone();
        let queue = state.haze_pending.clone();
        cx.spawn(async move |this, cx| {
            // Keep draining even if navigation drops this visualizer. Its final
            // scrub value still belongs in the saved venue.
            loop {
                let Some(value) = queue.borrow_mut().take() else {
                    break;
                };
                let Ok(pending) =
                    this.update(cx, |this, _| this.library.set_venue_haze(&venue, value))
                else {
                    break;
                };
                let result = pending.await;
                this.update(cx, |this, cx| {
                    let Some(state) = this
                        .visualizer_mut()
                        .filter(|s| Rc::ptr_eq(&s.haze_pending, &queue))
                    else {
                        return;
                    };
                    state.haze_saving = queue.borrow().is_some();
                    if !state.haze_saving {
                        state.haze_error = result
                            .err()
                            .map(|error| format!("Could not save haze: {error}"));
                    }
                    cx.notify();
                })
                .ok();
                if queue.borrow().is_none() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Persist one scrubbed device setting, coalescing the scrub: while a
    /// write is in flight only the newest value per key waits behind it.
    fn save_view_setting(&mut self, key: &'static str, value: String, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.view_setting_error = None;
        state.view_setting_pending.borrow_mut().insert(key, value);
        if state.view_setting_saving {
            return;
        }
        state.view_setting_saving = true;
        let queue = state.view_setting_pending.clone();
        cx.spawn(async move |this, cx| loop {
            let Some((key, value)) = queue.borrow_mut().pop_first() else {
                break;
            };
            let Ok(pending) = this.update(cx, |this, _| this.library.set_setting(key, &value))
            else {
                break;
            };
            let result = pending.await;
            this.update(cx, |this, cx| {
                let Some(state) = this
                    .visualizer_mut()
                    .filter(|s| Rc::ptr_eq(&s.view_setting_pending, &queue))
                else {
                    return;
                };
                state.view_setting_saving = !queue.borrow().is_empty();
                if let Err(error) = result {
                    state.view_setting_error = Some(format!("Could not save {key}: {error}"));
                }
                cx.notify();
            })
            .ok();
            if queue.borrow().is_empty() {
                break;
            }
        })
        .detach();
    }
}

fn environment_card(environment: VenueEnvironment, app: &Entity<Luma>) -> AnyElement {
    let mut track = float::segmented().w_full();
    for (name, indoor) in [("Indoor", true), ("Outdoor", false)] {
        let app = app.clone();
        track = track.child(
            float::segment(
                name,
                matches!(environment, VenueEnvironment::Indoor { .. }) == indoor,
                name,
            )
            .id(gpui::ElementId::Name(format!("environment-{name}").into()))
            .on_click(move |_, _, cx| {
                // Switching modes keeps neither scalar: the other mode's
                // dial is a different quantity, and a house level read as
                // an elevation is a sunset at one degree. Each mode opens
                // at its own default — house at full, sun at mid-morning.
                let chosen = if indoor {
                    VenueEnvironment::default()
                } else {
                    VenueEnvironment::outdoor(ENVIRONMENT_DEFAULT_SUN_DEG)
                };
                app.update(cx, |this, cx| this.set_visualizer_environment(chosen, cx));
            })
            .agent_node(Role::Toggle, name),
        );
    }
    track
        .agent_node(Role::Card, "Environment")
        .into_any_element()
}
const ENVIRONMENT_DEFAULT_SUN_DEG: f32 = 40.;

/// The sky's cloud cover, as one choice of five. `None` indoors, where there
/// is no sky.
///
/// A column rather than a row: five labels do not fit across the panel, and
/// a clipped "Fair weather" reads as a different word. It writes the venue's
/// environment and keeps the sun where it is.
fn cloud_rows(environment: VenueEnvironment, app: &Entity<Luma>) -> Option<Div> {
    if !matches!(environment, VenueEnvironment::Outdoor { .. }) {
        return None;
    }
    let chosen = environment.clouds();
    let mut track = float::segmented()
        .w_full()
        .h_auto()
        .flex_col()
        .items_stretch();
    for clouds in scene_desc::CloudCover::ALL {
        let app = app.clone();
        let label = clouds.label();
        track = track.child(
            float::segment(label, clouds == chosen, label)
                .flex_none()
                .h(px(luma_ui::CONTROL_HEIGHT - 4.))
                .id(gpui::ElementId::Name(format!("clouds-{label}").into()))
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        let Some(now) = this.visualizer_mut().map(|s| s.venue_environment()) else {
                            return;
                        };
                        this.set_visualizer_environment(now.with_clouds(clouds), cx);
                    });
                })
                .agent_node(Role::Toggle, label),
        );
    }
    Some(
        div().child(track).child(
            div()
                .size_0()
                .overflow_hidden()
                .agent_node(Role::Text, format!("Clouds = {}", chosen.label())),
        ),
    )
}

/// What the ground is made of, as one choice among the floors this kind of
/// venue offers: ground outdoors, stage and hall floors indoors.
///
/// A column like [`cloud_rows`], for the same reason. It writes the venue's
/// environment and keeps the light and the sky as they are.
fn floor_rows(environment: VenueEnvironment, app: &Entity<Luma>) -> Div {
    let chosen = environment.floor();
    let indoor = matches!(environment, VenueEnvironment::Indoor { .. });
    let mut track = float::segmented()
        .w_full()
        .h_auto()
        .flex_col()
        .items_stretch();
    for &floor in scene_desc::Floor::options(indoor) {
        let app = app.clone();
        let label = floor.label();
        track = track.child(
            float::segment(label, floor == chosen, label)
                .flex_none()
                .h(px(luma_ui::CONTROL_HEIGHT - 4.))
                .id(gpui::ElementId::Name(format!("floor-{label}").into()))
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        let Some(now) = this.visualizer_mut().map(|s| s.venue_environment()) else {
                            return;
                        };
                        this.set_visualizer_environment(now.with_floor(floor), cx);
                    });
                })
                .agent_node(Role::Toggle, label),
        );
    }
    div().child(track).child(
        div()
            .size_0()
            .overflow_hidden()
            .agent_node(Role::Text, format!("Floor = {}", chosen.label())),
    )
}

/// A titled group of rows on the View settings card: the card's quiet
/// [`float::label`] over the rows it names.
pub(super) fn section(title: &'static str, rows: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(float::label(title))
        .child(rows)
}

/// Where an open-air venue's sun is, as two rows in degrees. `None` indoors,
/// where there is no sun.
///
/// Both write the venue's environment, the same synced value the dock's time
/// of day slider writes, and each keeps the other angle as it is.
fn sun_rows(environment: VenueEnvironment, app: &Entity<Luma>) -> Option<Div> {
    if !matches!(environment, VenueEnvironment::Outdoor { .. }) {
        return None;
    }
    let elevation = app.clone();
    let azimuth = app.clone();
    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(super::scrub_row(
                "Sun elevation (°)",
                environment.sun_elevation_deg(),
                -90.0..=90.0,
                1.0,
                1.0,
                move |deg, cx| {
                    elevation.update(cx, |this, cx| {
                        let Some(now) = this.visualizer_mut().map(|s| s.venue_environment()) else {
                            return;
                        };
                        this.set_visualizer_environment(
                            VenueEnvironment::outdoor(deg)
                                .with_sun_azimuth(now.sun_azimuth_deg())
                                .with_clouds(now.clouds())
                                .with_floor(now.floor()),
                            cx,
                        );
                    });
                },
            ))
            .child(super::scrub_row(
                "Sun azimuth (°)",
                environment.sun_azimuth_deg(),
                0.0..=360.0,
                1.0,
                1.0,
                move |deg, cx| {
                    azimuth.update(cx, |this, cx| {
                        let Some(now) = this.visualizer_mut().map(|s| s.venue_environment()) else {
                            return;
                        };
                        this.set_visualizer_environment(now.with_sun_azimuth(deg), cx);
                    });
                },
            )),
    )
}
