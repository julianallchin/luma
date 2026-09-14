//! Compact environment dock shared by every visualizer.
use super::*;
use luma_ui::node::AgentNode as _;
use luma_ui::{float, glass};

/// Persistent clocks survive render calls and retain the popover for its exit.
#[derive(Default)]
pub(super) struct DockMotion {
    popup: Transition,
    tooltip: Transition,
    pub(super) switches: [Transition; 4],
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
            popup: transition(),
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
    let camera_bounds = Rc::new(std::cell::Cell::new(gpui::Bounds::default()));
    let measured_camera = camera_bounds.clone();
    let openness = state
        .settings_motion
        .borrow_mut()
        .popup
        .sample(state.settings_open);
    let mut content = float::popover_card()
        .w(px(268.))
        .p(px(14.))
        .gap(px(12.))
        .child(float::label("View settings"))
        .child(environment_card(state.venue_environment(), app))
        .child(float::divider())
        .child(super::view_controls(state, app));
    for error in [
        &state.environment_error,
        &state.haze_error,
        &state.view_setting_error,
    ]
    .into_iter()
    .flatten()
    {
        content = content.child(float::error_row(error.clone()));
    }
    let camera = div()
        .id("visualizer-settings")
        .relative()
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .bg(glass::wash(0.10 * openness))
        .hover(|s| s.bg(glass::wash(0.12)))
        .child(
            gpui::svg()
                .path("nucleo/camera.svg")
                .size(px(18.))
                .text_color(ladder::foreground_alpha(0.8)),
        )
        .child(
            canvas(
                move |bounds, _, _| measured_camera.set(bounds),
                |_, (), _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .on_click(move |_, _, cx| {
            toggle.update(cx, |this, cx| {
                if let Some(state) = this.visualizer_mut() {
                    state.settings_open = !state.settings_open;
                }
                cx.notify();
            });
        })
        .agent_node(Role::Toggle, "Render settings")
        .agent_focused(state.settings_open);
    div()
        .absolute()
        .right(px(16.))
        .bottom(TOOLBAR_OVERLAY_BOTTOM)
        .child(float::frosted_card(
            float::popover_card()
                .w(px(40.))
                .p(px(6.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .occlude()
                .child(camera)
                .child(light_slider(state, app)),
        ))
        .when(state.settings_open || openness > 0., |d| {
            let card = content
                .id("view-settings-card")
                .occlude()
                .on_mouse_down_out(move |event, _, cx| {
                    if camera_bounds.get().contains(&event.position) {
                        return;
                    }
                    close.update(cx, |this, cx| {
                        if let Some(state) = this.visualizer_mut() {
                            state.settings_open = false;
                        }
                        cx.notify();
                    });
                })
                .agent_node(Role::Card, "Render settings");
            let animated = div()
                .pr(px(8.))
                .relative()
                .left(px(4. * (1. - openness)))
                .opacity(openness)
                .child(float::frosted_card(card));
            d.child(
                div().absolute().left_0().bottom_0().size_0().child(
                    gpui::deferred(
                        gpui::anchored()
                            .anchor(gpui::Anchor::BottomRight)
                            .child(animated),
                    )
                    .priority(1),
                ),
            )
        })
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
                        VenueEnvironment::outdoor(fraction * 180. - 90.)
                    },
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

    /// A view toggle: applied to the lab now, persisted where it is durable.
    ///
    /// The three tiers of this panel meet here. Haze is **venue truth** and goes
    /// on the venue row; grid and gizmos are **local device settings** and follow
    /// the operator between rooms; fixture shadows is a session dial and outlives
    /// nothing.
    pub(super) fn toggle_view_control(&mut self, control: LabToggle, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.render_lab.toggle(control);
        let (grid, gizmos) = (
            state.render_lab.grid_enabled,
            state.render_lab.gizmos_enabled,
        );
        match control {
            LabToggle::Haze => self.save_venue_haze(cx),
            // Straight through `set_setting`: it is already FIFO-ordered, and a
            // toggle cannot emit faster than a hand can click, so the
            // coalescing the scrubs need would buy nothing here.
            LabToggle::Grid => self.write_view_setting("stage_grid", grid.to_string(), cx),
            LabToggle::Gizmos => self.write_view_setting("stage_gizmos", gizmos.to_string(), cx),
            LabToggle::FixtureShadows => {}
        }
        cx.notify();
    }

    /// A view scrub: applied to the lab now, persisted where it is durable.
    pub(super) fn set_view_value(&mut self, control: LabValue, value: f32, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.render_lab.set(control, value);
        match control {
            LabValue::RenderScale => self.save_render_scale(cx),
            _ => self.save_venue_haze(cx),
        }
        cx.notify();
    }

    /// One local view setting, written once.
    ///
    /// Deliberately *not* `Luma::write_setting`: that one re-reads the whole
    /// settings record afterwards to repaint the Settings screen, which is a
    /// screen this panel is not. The lab already holds the new value, so the
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

    /// Persist the haze the lab now holds.
    ///
    /// The lab is the authority — [`RenderLab::toggle`] and [`RenderLab::set`]
    /// have already sanitized it — so this takes no value. Same guard as
    /// [`Self::save_visualizer_environment`], for the same reason: a dragged
    /// density emits a value per pointer move.
    fn save_venue_haze(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        let haze = state.render_lab.haze;
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

    /// Persist the render percent the lab now holds, coalescing the scrub.
    fn save_render_scale(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.visualizer_mut() else {
            return;
        };
        state.view_setting_error = None;
        *state.render_scale_pending.borrow_mut() = Some(state.render_lab.render_scale_percent);
        if state.render_scale_saving {
            return;
        }
        state.render_scale_saving = true;
        let queue = state.render_scale_pending.clone();
        cx.spawn(async move |this, cx| loop {
            let Some(percent) = queue.borrow_mut().take() else {
                break;
            };
            let Ok(pending) = this.update(cx, |this, _| {
                this.library
                    .set_setting("render_scale", &percent.to_string())
            }) else {
                break;
            };
            let result = pending.await;
            this.update(cx, |this, cx| {
                let Some(state) = this
                    .visualizer_mut()
                    .filter(|s| Rc::ptr_eq(&s.render_scale_pending, &queue))
                else {
                    return;
                };
                state.render_scale_saving = queue.borrow().is_some();
                if !state.render_scale_saving {
                    state.view_setting_error = result
                        .err()
                        .map(|error| format!("Could not save render_scale: {error}"));
                }
                cx.notify();
            })
            .ok();
            if queue.borrow().is_none() {
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
