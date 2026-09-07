//! Shared controls for the room shown by every visualizer.
use super::*;
use luma_ui::float;
use luma_ui::node::AgentNode as _;

pub(super) fn trigger(state: &Visualizer, app: &Entity<Luma>) -> AnyElement {
    let toggle = app.clone();
    let close = app.clone();
    let mut content = float::popover_card()
        .w(px(280.0))
        .p(px(12.0))
        .gap(px(12.0))
        .child(float::label("Render settings"))
        .child(environment_card(state.venue_environment(), app))
        .child(super::renderer_lab_trigger(state, app));
    if let Some(error) = &state.environment_error {
        content = content.child(luma_ui::plate(error.clone(), ladder::danger()));
    }
    div()
        .relative()
        .flex_none()
        .child(
            float::btn("View", "visualizer-settings")
                .id("visualizer-settings")
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |this, cx| {
                        if let Some(state) = this.visualizer_mut() {
                            state.settings_open = !state.settings_open;
                        }
                        cx.notify();
                    })
                })
                .agent_node(Role::Toggle, "Render settings")
                .agent_focused(state.settings_open),
        )
        .when(state.settings_open, |d| {
            d.child(float::anchored_below(
                "visualizer-settings-popover",
                luma_ui::CONTROL_HEIGHT,
                float::Dismiss::on_press_out(move |_, cx| {
                    close.update(cx, |this, cx| {
                        if let Some(state) = this.visualizer_mut() {
                            state.settings_open = false;
                        }
                        cx.notify();
                    })
                }),
                content
                    .agent_node(Role::Card, "Render settings")
                    .into_any_element(),
            ))
        })
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
}

/// Venue environment, independent of score playback.
fn environment_card(environment: VenueEnvironment, app: &Entity<Luma>) -> AnyElement {
    let mut track = float::segmented().w(px(ENVIRONMENT_MODE_W));
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
    let dial = match environment {
        VenueEnvironment::Indoor { .. } => {
            let app = app.clone();
            float::field_row(
                "House",
                float::scrub(
                    "visualizer-house-level",
                    f64::from(environment.house_level()),
                    0.0,
                    1.0,
                    0.05,
                    ENVIRONMENT_DIAL_W,
                    move |value, _, cx| {
                        app.update(cx, |this, cx| {
                            #[allow(clippy::cast_possible_truncation)]
                            this.set_visualizer_environment(
                                VenueEnvironment::indoor(value as f32),
                                cx,
                            );
                        });
                    },
                ),
            )
        }
        VenueEnvironment::Outdoor { .. } => {
            let app = app.clone();
            float::field_row(
                "Sun",
                float::scrub(
                    "visualizer-sun-elevation",
                    f64::from(environment.sun_elevation_deg()),
                    -90.0,
                    90.0,
                    1.0,
                    ENVIRONMENT_DIAL_W,
                    move |value, _, cx| {
                        app.update(cx, |this, cx| {
                            #[allow(clippy::cast_possible_truncation)]
                            this.set_visualizer_environment(
                                VenueEnvironment::outdoor(value as f32),
                                cx,
                            );
                        });
                    },
                ),
            )
        }
    };
    div()
        .child(
            div()
                .flex()
                .flex_row()
                .items_end()
                .gap(px(8.0))
                .child(float::field_row("Room", track))
                .child(dial)
                .agent_node(Role::Card, "Environment"),
        )
        .into_any_element()
}

/// Wide enough for "Outdoor" twice over, and narrow enough that the card is
/// chrome in a corner rather than a panel.
const ENVIRONMENT_MODE_W: f32 = 148.0;

/// The dial's box. The number is the whole control, so it is sized to the
/// widest number either mode prints — a signed two-digit elevation.
const ENVIRONMENT_DIAL_W: f32 = 56.0;

/// Where an open-air venue's sun starts: high enough that the room is lit and
/// low enough that everything in it still casts a shadow with a direction.
const ENVIRONMENT_DEFAULT_SUN_DEG: f32 = 40.0;
