//! The venue's lasers: the window its clips draw in, what its projectors
//! are, what plays each one and where each may send light
//! (`docs/design/lasers.md`).
//!
//! Most rigs hang several of one projector, so what a projector is, its
//! diodes, scanner and beam, is set once for the venue. A projector's own
//! card holds what is only ever its own: the DAC it is wired to, how its
//! picture is turned for the way it hangs, and its limit. One that differs
//! from the rest is given a kind of its own there.
//!
//! An edit shows in the room as it is made and is written when it is let
//! go, as one step of the stage's undo.
//!
//! Laser output is the switch at the top. It is off at every launch, and
//! nothing reaches a DAC until someone turns it on here.

mod window;

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    canvas, div, point, px, relative, AnyElement, Bounds, Context, Entity, FontWeight, MouseButton,
    PathBuilder, Pixels, Point, SharedString,
};
use luma_lib::laser::{Key, Preset, Projector, Settings, Window, FLICKER_RATE};
use luma_lib::models::lasers::{LaserDevice, LaserLink, LaserRoute};
use luma_ui::float::{Dismiss, RowState};
use luma_ui::icons::IconName;
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::scrub_number::ScrubNumber;

use super::{Patch, View};
use crate::confirm::{Action, Confirm};
use crate::library::{LaserEdits, Lasers};
use crate::Luma;

/// The page's column: as wide as a projector's setup laid side by side.
const PAGE: f32 = 1160.0;
const PAD: f32 = 16.0;
const GAP: f32 = 12.0;
/// The name before a row of a settings table.
const W_NAME: f32 = 92.0;
/// A number with its unit: every cell of a table is this wide, so numbers
/// stacked in rows stand in columns.
const CELL: f32 = 104.0;
/// The limit editor's square: the projector's whole scan.
const LIMIT_BOX: f32 = 220.0;
const HANDLE: f32 = 10.0;
/// How often the page asks how the DACs are doing.
const POLL: Duration = Duration::from_secs(1);
/// The limit a projector starts with when it is given one: its scan, a
/// little in from every edge.
const FIRST_LIMIT: [[f64; 2]; 4] = [[-0.9, -0.9], [0.9, -0.9], [0.9, 0.9], [-0.9, 0.9]];
/// What the venue's shared kind is called where a projector's name goes.
const EVERY: &str = "Projectors";

/// What the page holds beside the venue's data.
#[derive(Default)]
pub(crate) struct State {
    /// The projector whose setup is open.
    more: Option<String>,
    /// Whether the shared kind's details are open.
    details: bool,
    /// The projector whose DAC list is open.
    binding: Option<String>,
    /// The projector showing its test pattern, here and on its DAC.
    testing: Option<String>,
    /// Edits let go of and not yet written: the newest of each.
    edits: LaserEdits,
    /// A write is on its way; the next waits for it.
    writing: bool,
    /// The venue as it was before the writes in hand: what undo restores.
    undo: Option<crate::stage::Snapshot>,
    /// The window editor.
    window: window::State,
    /// Where the limit editor's square was last drawn.
    limit_box: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// The corner being dragged: its projector and which.
    limit_drag: Option<(String, usize)>,
    /// The page is asking the DACs how they are.
    polling: bool,
}

impl Patch {
    fn laser_data(&self) -> Option<&Lasers> {
        self.data.as_ref().map(|data| &data.lasers)
    }

    /// Whether the venue has a projector, and so a Lasers view.
    pub(crate) fn has_lasers(&self) -> bool {
        self.laser_data()
            .is_some_and(|lasers| !lasers.rig.is_empty())
    }

    fn projector_name(&self, id: &str) -> SharedString {
        self.row(id)
            .map(|row| row.label.clone().unwrap_or_else(|| row.model.clone()))
            .unwrap_or_else(|| "Projector".into())
            .into()
    }
}

// ---------------------------------------------------------------------------
// Acts
// ---------------------------------------------------------------------------

impl Luma {
    /// Change the venue's lasers as they are held here, and show the change
    /// in the room. `commit` also writes it; without, it is a change still
    /// in hand, a number mid-scrub or a window mid-drag, and only shown.
    fn change_lasers(
        &mut self,
        venue_id: String,
        commit: bool,
        edit: impl FnOnce(&mut Lasers, &mut LaserEdits),
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        let Some(data) = state.data.as_mut() else {
            return;
        };
        let mut shown_only = LaserEdits::default();
        edit(
            &mut data.lasers,
            if commit {
                &mut state.lasers.edits
            } else {
                &mut shown_only
            },
        );
        let rig = data.lasers.rig.clone();
        if let Some(stage) = self
            .visualizer_mut()
            .filter(|stage| stage.venue_id == venue_id)
        {
            stage.set_lasers(rig);
        }
        if commit {
            self.write_lasers(venue_id, cx);
        }
        cx.notify();
    }

    /// Write what is queued, one write at a time, then read the venue again:
    /// the stage holds the venue's graph, and what it holds is what its next
    /// undo step is taken from. The writes between two reads are one step.
    fn write_lasers(&mut self, venue_id: String, cx: &mut Context<Self>) {
        // What these writes replace is what undo restores.
        let before = self.build_state().map(crate::stage::Build::snapshot);
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        if state.lasers.writing || state.lasers.edits.is_empty() {
            return;
        }
        state.lasers.writing = true;
        let edits = std::mem::take(&mut state.lasers.edits);
        if edits.changes_graph() && state.lasers.undo.is_none() {
            state.lasers.undo = before;
        }
        let pending = self.library.write_lasers(&venue_id, edits);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let failed = result.is_err();
                if let Err(error) = result {
                    this.report_failure("The laser change was not saved", error, cx);
                }
                let Some(state) = this.patch_mut(&venue_id) else {
                    return;
                };
                state.lasers.writing = false;
                if failed {
                    state.lasers.undo = None;
                } else if !state.lasers.edits.is_empty() {
                    return this.write_lasers(venue_id, cx);
                }
                let step = this
                    .patch_mut(&venue_id)
                    .and_then(|state| state.lasers.undo.take());
                if let (Some(step), Some(build)) = (step, this.build_mut()) {
                    build.remember(step);
                }
                if this.visualizer.is_some() {
                    this.reload_stage(cx);
                } else {
                    this.reload_patch(venue_id, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Change some settings: a projector's, or with no projector the ones
    /// the venue's projectors share.
    fn set_laser_settings(
        &mut self,
        venue_id: String,
        projector: Option<String>,
        changes: Vec<(Key, f64)>,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        self.change_lasers(
            venue_id,
            commit,
            move |lasers, edits| {
                let rig = &mut lasers.rig;
                let queued = edits.settings.entry(projector.clone()).or_default();
                for (key, value) in changes {
                    let mut stored = Settings::default();
                    stored.set(key, value);
                    queued.insert(key.param().to_owned(), stored.get(key));
                    match &projector {
                        Some(id) => {
                            if let Some(held) = rig.projectors.iter_mut().find(|p| &p.id == id) {
                                held.settings.set(key, value);
                            }
                        }
                        // Every projector that follows the venue's kind.
                        None => {
                            rig.shared.set(key, value);
                            for held in rig.projectors.iter_mut().filter(|p| !p.own) {
                                held.settings.set(key, value);
                            }
                        }
                    }
                }
            },
            cx,
        );
    }

    /// Give a projector a kind of its own, starting from the venue's, or
    /// have it follow the venue's again.
    fn set_laser_own(
        &mut self,
        venue_id: String,
        projector: String,
        own: bool,
        cx: &mut Context<Self>,
    ) {
        self.change_lasers(
            venue_id,
            true,
            move |lasers, edits| {
                let shared = lasers.rig.shared;
                if let Some(held) = lasers.rig.projectors.iter_mut().find(|p| p.id == projector) {
                    held.own = own;
                    if !own {
                        for key in Key::ALL.into_iter().filter(|key| !key.is_own()) {
                            held.settings.set(key, shared.get(key));
                        }
                    }
                    edits.settings.remove(&Some(projector.clone()));
                    edits.own.insert(projector, own);
                }
            },
            cx,
        );
    }

    /// Set the venue's window, or with `None` fit it to the projectors.
    fn set_laser_window(
        &mut self,
        venue_id: String,
        window: Option<Window>,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        if window.is_some_and(|window| window.check().is_err()) {
            return;
        }
        self.change_lasers(
            venue_id,
            commit,
            move |lasers, edits| {
                lasers.rig.window = window;
                edits.window = Some(window);
            },
            cx,
        );
    }

    pub(crate) fn set_laser_limit(
        &mut self,
        venue_id: String,
        projector: String,
        limit: Option<Vec<[f64; 2]>>,
        cx: &mut Context<Self>,
    ) {
        self.change_lasers(
            venue_id,
            true,
            move |lasers, edits| {
                if let Some(held) = lasers.rig.projectors.iter_mut().find(|p| p.id == projector) {
                    held.limit.clone_from(&limit);
                    edits.limits.insert(projector, limit);
                }
            },
            cx,
        );
    }

    /// Move the corner in hand to where the pointer is. Held here until the
    /// button comes up: a limit is written once, where it was let go.
    fn drag_laser_limit(&mut self, venue_id: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        let (Some((projector, corner)), Some(bounds)) = (
            state.lasers.limit_drag.clone(),
            *state.lasers.limit_box.borrow(),
        ) else {
            return;
        };
        let place = galvo_at(bounds, at);
        let limit = state
            .data
            .as_mut()
            .and_then(|data| {
                data.lasers
                    .rig
                    .projectors
                    .iter_mut()
                    .find(|p| p.id == projector)
            })
            .and_then(|projector| projector.limit.as_mut());
        if let Some(held) = limit.and_then(|limit| limit.get_mut(corner)) {
            *held = place;
            cx.notify();
        }
    }

    fn drop_laser_limit(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let dropped = self.patch_mut(&venue_id).and_then(|state| {
            let (projector, _) = state.lasers.limit_drag.take()?;
            let limit = state.laser_data()?.rig.projector(&projector)?.limit.clone();
            Some((projector, limit))
        });
        if let Some((projector, limit)) = dropped {
            self.set_laser_limit(venue_id, projector, limit, cx);
        }
    }

    fn set_laser_output(&mut self, venue_id: String, on: bool, cx: &mut Context<Self>) {
        let pending = self.library.set_laser_output(on);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| match result {
                Ok(output) => this.laser_output_landed(&venue_id, output, None, cx),
                Err(error) => this.report_failure("Laser output did not change", error, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Turn laser output off from anywhere: the status bar's press.
    pub(crate) fn stop_lasers(&mut self, cx: &mut Context<Self>) {
        let pending = self.library.set_laser_output(false);
        cx.spawn(async move |this, cx| {
            if pending.await.is_ok() {
                this.update(cx, |this, cx| {
                    this.laser_output = false;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Show a projector's test pattern, in the room and on its DAC, or with
    /// `None` go back to the show.
    fn test_laser(&mut self, venue_id: String, projector: Option<String>, cx: &mut Context<Self>) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        state.lasers.testing.clone_from(&projector);
        let on_a_dac = state
            .laser_data()
            .is_some_and(|lasers| lasers.output.is_some());
        if let Some(stage) = self.visualizer_mut() {
            stage.set_laser_test(projector.clone());
        }
        cx.notify();
        if !on_a_dac {
            return;
        }
        let pending = self.library.set_laser_test(projector);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| match result {
                Ok(output) => this.laser_output_landed(&venue_id, output, None, cx),
                Err(error) => this.report_failure("The test pattern did not change", error, cx),
            })
            .ok();
        })
        .detach();
    }

    fn laser_output_landed(
        &mut self,
        venue_id: &str,
        output: luma_lib::models::lasers::LaserOutput,
        devices: Option<Vec<LaserDevice>>,
        cx: &mut Context<Self>,
    ) {
        self.laser_output = output.on;
        if let Some(lasers) = self
            .patch_mut(venue_id)
            .and_then(|state| state.data.as_mut())
            .map(|data| &mut data.lasers)
        {
            lasers.output = Some(output);
            if let Some(devices) = devices {
                lasers.devices = devices;
            }
        }
        cx.notify();
    }

    fn bind_laser(
        &mut self,
        venue_id: String,
        projector: String,
        device: Option<LaserDevice>,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.lasers.binding = None;
        }
        type Write = std::pin::Pin<Box<dyn Future<Output = Result<(), crate::LibraryError>>>>;
        let pending: Write = match &device {
            Some(device) => Box::pin(self.library.bind_laser(&projector, device)),
            None => Box::pin(self.library.unbind_laser(&projector)),
        };
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.report_failure("The projector's DAC did not change", error, cx);
                }
                this.reload_patch(venue_id, cx);
            })
            .ok();
        })
        .detach();
    }

    fn toggle_laser_panel(
        &mut self,
        venue_id: String,
        toggle: impl FnOnce(&mut State),
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            toggle(&mut state.lasers);
        }
        cx.notify();
    }

    /// Ask how the DACs are doing once a second while the Lasers view is up:
    /// a link comes and goes on its own, and nothing else would say so.
    pub(crate) fn watch_lasers(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        if state.lasers.polling {
            return;
        }
        state.lasers.polling = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL).await;
            let Ok(pending) = this.update(cx, |this, _| {
                let shown = this
                    .patch_mut(&venue_id)
                    .is_some_and(|state| state.view == View::Lasers);
                shown.then(|| this.library.laser_output())
            }) else {
                return;
            };
            let Some(pending) = pending else {
                this.update(cx, |this, _| {
                    if let Some(state) = this.patch_mut(&venue_id) {
                        state.lasers.polling = false;
                    }
                })
                .ok();
                return;
            };
            if let Some((output, devices)) = pending.await {
                this.update(cx, |this, cx| {
                    this.laser_output_landed(&venue_id, output, Some(devices), cx);
                })
                .ok();
            }
        })
        .detach();
    }
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

pub(super) fn lasers(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    let Some(lasers) = state.laser_data() else {
        return div().flex_1().into_any_element();
    };
    div()
        .id("venue-lasers")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(PAGE))
                .px(px(PAD))
                .pt(px(4.0))
                .pb(px(24.0))
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(output(state, lasers, app))
                .child(window_card(state, lasers, app))
                .child(kind_card(state, lasers, app))
                .children(
                    lasers
                        .rig
                        .projectors
                        .iter()
                        .map(|projector| projector_card(state, lasers, projector, app)),
                ),
        )
        .agent_node(Role::Card, "Lasers")
        .into_any_element()
}

// ---------------------------------------------------------------------------
// The page's few shapes
// ---------------------------------------------------------------------------

/// A section of the page: the output, the window, one projector.
fn card() -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .rounded(px(luma_ui::radius::PANEL))
        .bg(luma_ui::glass::ink(0.035))
        .border_1()
        .border_color(luma_ui::glass::hairline(0.08))
}

/// A card's top line: what it is, and room after it for what it can do.
fn head(title: impl Into<SharedString>) -> gpui::Div {
    div()
        .h(px(44.0))
        .px(px(PAD))
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            div()
                .flex_none()
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .child(title.into()),
        )
}

fn quiet(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(11.5))
        .line_height(px(16.0))
        .text_color(ladder::foreground_alpha(0.55))
        .child(text.into())
}

/// A button on the page: a plate, so it reads as one at rest.
fn action(label: &'static str, id: String) -> gpui::Stateful<gpui::Div> {
    luma_ui::float::chip()
        .child(label)
        .id(SharedString::from(id))
}

/// A state in a word, with a dot that says how it is going.
fn status(text: impl Into<SharedString>, dot: gpui::Hsla) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .min_w_0()
        .child(div().flex_none().size(px(6.0)).rounded_full().bg(dot))
        .child(
            div()
                .truncate()
                .text_size(px(11.5))
                .text_color(ladder::foreground_alpha(0.62))
                .child(text.into()),
        )
}

/// A number with its unit, in a cell as wide as every other. Dragged, it
/// moves a step every few pixels however wide its range is; clicked, it is
/// typed. `show` is the value in hand, `set` the one let go of.
#[allow(clippy::too_many_arguments)]
fn number(
    id: String,
    unit: &'static str,
    value: f64,
    [min, max]: [f64; 2],
    step: f64,
    show: impl Fn(f64, &mut gpui::App) + 'static,
    set: impl Fn(f64, &mut gpui::App) + 'static,
) -> gpui::Div {
    div().flex_none().w(px(CELL)).child(
        ScrubNumber::new(
            id,
            value,
            min..=max,
            step,
            CELL - GAP,
            unit,
            move |value, _, cx| {
                set(value, cx);
            },
        )
        .on_preview(move |value, _, cx| show(value, cx)),
    )
}

/// A control under its name.
fn field(name: &'static str, control: impl IntoElement) -> gpui::Div {
    luma_ui::float::field_row(name, control).flex_none()
}

/// A row of a settings table: its name, then its cells.
fn line(name: &'static str) -> gpui::Div {
    div()
        .min_h(px(luma_ui::CONTROL_HEIGHT))
        .flex()
        .items_center()
        .gap(px(GAP))
        .child(luma_ui::float::label(name).w(px(W_NAME)))
}

/// A table of settings under its heading.
fn group(title: &'static str) -> gpui::Div {
    div().flex().flex_col().gap(px(8.0)).child(
        div()
            .pb(px(2.0))
            .text_size(px(12.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(ladder::foreground_alpha(0.9))
            .child(title),
    )
}

/// A chip that opens what is under it, and says which way it is.
fn disclosure(label: &'static str, open: bool, id: String) -> gpui::Stateful<gpui::Div> {
    luma_ui::float::chip()
        .child(label)
        .child(
            gpui_component::Icon::new(if open {
                IconName::ChevronUp
            } else {
                IconName::ChevronDown
            })
            .size(px(10.0))
            .text_color(ladder::foreground_alpha(0.55)),
        )
        .id(SharedString::from(id))
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// The switch, and what it does.
fn output(state: &Patch, lasers: &Lasers, app: &Entity<Luma>) -> AnyElement {
    let Some(output) = &lasers.output else {
        let saying = "Laser output is not running on this machine.";
        return card()
            .child(head("Laser output").child(quiet(saying).agent_node(Role::Text, saying)))
            .into_any_element();
    };
    let on = output.on;
    let flipped = app.clone();
    let venue = state.venue_id.clone();
    let bound = lasers
        .rig
        .projectors
        .iter()
        .filter(|projector| {
            lasers
                .routes
                .iter()
                .any(|route| route.fixture_id == projector.id)
        })
        .count();
    let saying: SharedString = if on {
        format!(
            "On: {} sent to {}.",
            crate::patch::plural(bound, "projector"),
            if bound == 1 { "its DAC" } else { "their DACs" }
        )
        .into()
    } else {
        "Off. Nothing is sent to a DAC until this is on; it is off every time Luma starts.".into()
    };
    card()
        .child(
            head("Laser output")
                .child(
                    quiet(saying.clone())
                        .flex_1()
                        .agent_node(Role::Text, saying),
                )
                .child(
                    div()
                        .id("laser-output")
                        .flex_none()
                        .cursor_pointer()
                        .child(luma_ui::float::switch(if on { 1.0 } else { 0.0 }))
                        .on_click(move |_, _, cx| {
                            let venue = venue.clone();
                            flipped.update(cx, |this, cx| this.set_laser_output(venue, !on, cx));
                        })
                        .agent_node(Role::Toggle, "Laser output")
                        .agent_focused(on),
                ),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

/// The window every laser clip draws in: its graph, and its four edges as
/// numbers.
fn window_card(state: &Patch, lasers: &Lasers, app: &Entity<Luma>) -> AnyElement {
    let shown = lasers.rig.window();
    let set = lasers.rig.window.is_some();
    let edge = |index: usize, name: &'static str, range: [f64; 2]| {
        let value = [shown.turn[0], shown.turn[1], shown.tilt[0], shown.tilt[1]][index];
        let to = move |value: f64| {
            let mut window = shown;
            match index {
                0 => window.turn[0] = value,
                1 => window.turn[1] = value,
                2 => window.tilt[0] = value,
                _ => window.tilt[1] = value,
            }
            window
        };
        let change = |commit: bool| {
            let (app, venue) = (app.clone(), state.venue_id.clone());
            move |value: f64, cx: &mut gpui::App| {
                let venue = venue.clone();
                app.update(cx, |this, cx| {
                    this.set_laser_window(venue, Some(to(value)), commit, cx);
                });
            }
        };
        field(
            name,
            number(
                format!("Laser window {name}"),
                "°",
                value.round(),
                range,
                1.,
                change(false),
                change(true),
            ),
        )
    };
    let fit = app.clone();
    let venue = state.venue_id.clone();
    let saying = if set {
        "Where every laser clip draws: its canvas, as directions out from the stage."
    } else {
        "Fitted to the projectors: ahead into the house, just above level."
    };
    card()
        .child(
            head("Window")
                .child(quiet(saying).flex_1().agent_node(Role::Text, saying))
                .when(set, |head| {
                    head.child(
                        action("Fit to projectors", "laser-window-fit".into())
                            .on_click(move |_, _, cx| {
                                let venue = venue.clone();
                                fit.update(cx, |this, cx| {
                                    this.set_laser_window(venue, None, true, cx);
                                });
                            })
                            .agent_node(Role::Button, "Fit to projectors"),
                    )
                }),
        )
        .child(
            div()
                .px(px(PAD))
                .pb(px(PAD))
                .flex()
                .flex_wrap()
                .items_start()
                .gap(px(24.0))
                .child(window::editor(state, lasers, app))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.0))
                        .max_w(px(2.0 * CELL + GAP))
                        .child(
                            div()
                                .flex()
                                .gap(px(GAP))
                                .child(edge(0, "Left", [-180., 180.]))
                                .child(edge(1, "Right", [-180., 180.])),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(GAP))
                                .child(edge(2, "Bottom", [-90., 90.]))
                                .child(edge(3, "Top", [-90., 90.])),
                        )
                        .child(quiet(
                            "Drag the window to move it and its handles to resize it, or drag \
                             on the graph to draw a new one. The grey outlines are where each \
                             projector can reach.",
                        ))
                        .child(quiet(
                            "Alt resizes about the middle. Ctrl or Cmd lets go of the snapping.",
                        )),
                ),
        )
        .agent_node(Role::Row, "Laser window")
        .into_any_element()
}

// ---------------------------------------------------------------------------
// What a projector is
// ---------------------------------------------------------------------------

/// Whose kind a control edits: the venue's, or one projector's own.
#[derive(Clone)]
struct Kind {
    venue: String,
    /// `None` for the venue's, which every projector without its own
    /// follows.
    projector: Option<String>,
    /// The projector's name, or [`EVERY`].
    name: SharedString,
    settings: Settings,
    app: Entity<Luma>,
}

impl Kind {
    fn id(&self, part: &str) -> String {
        format!(
            "laser-{}-{part}",
            self.projector.as_deref().unwrap_or("every")
        )
    }

    /// Change settings, shown only or written too.
    fn change(&self, commit: bool) -> impl Fn(Vec<(Key, f64)>, &mut gpui::App) + 'static {
        let (app, venue, projector) =
            (self.app.clone(), self.venue.clone(), self.projector.clone());
        move |changes, cx| {
            let (venue, projector) = (venue.clone(), projector.clone());
            app.update(cx, |this, cx| {
                this.set_laser_settings(venue, projector, changes, commit, cx);
            });
        }
    }

    /// One setting as a number, shown times `scale`.
    fn number(
        &self,
        key: Key,
        reading: &str,
        unit: &'static str,
        scale: f64,
        step: f64,
    ) -> gpui::Div {
        let (show, set) = (self.change(false), self.change(true));
        number(
            format!("{} {reading}", self.name),
            unit,
            self.settings.get(key) * scale,
            key.range().map(|edge| edge * scale),
            step,
            move |value, cx| show(vec![(key, value / scale)], cx),
            move |value, cx| set(vec![(key, value / scale)], cx),
        )
    }

    /// One setting as a switch, under `word`.
    fn switch(&self, key: Key, word: &'static str, reading: &'static str) -> AnyElement {
        let on = self.settings.is_on(key);
        let set = self.change(true);
        div()
            .id(SharedString::from(self.id(key.param())))
            .flex_none()
            .w(px(CELL))
            .flex()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .child(luma_ui::float::switch(if on { 1.0 } else { 0.0 }))
            .child(quiet(word))
            .on_click(move |_, _, cx| set(vec![(key, if on { 0. } else { 1. })], cx))
            .agent_node(Role::Toggle, format!("{} {reading}", self.name))
            .agent_focused(on)
            .into_any_element()
    }

    /// The row that is the common case: a size to start from, how wide it
    /// scans, how fast, and how much of its power it may emit.
    fn row(&self) -> gpui::Div {
        let matched = self.settings.matches();
        let presets = luma_ui::float::segmented().children(Preset::ALL.map(|preset| {
            let label = preset.name();
            let set = self.change(true);
            luma_ui::float::segment(label, matched == Some(preset), label)
                .id(SharedString::from(self.id(label)))
                .on_click(move |_, _, cx| {
                    let to = Settings::preset(preset);
                    set(Settings::SHAPE.map(|key| (key, to.get(key))).to_vec(), cx);
                })
                .agent_node(Role::Toggle, format!("{} {label}", self.name))
                .agent_focused(matched == Some(preset))
        }));
        div()
            .flex()
            .flex_wrap()
            .items_end()
            .gap(px(GAP))
            .child(field("Preset", presets))
            .child(field(
                "Scan angle",
                self.number(Key::Angle, "scan angle", "°", 1., 1.),
            ))
            .child(field(
                "Scanner",
                self.number(Key::Speed, "scanner", "kpps", 1., 1.),
            ))
            .child(field(
                "Power cap",
                self.number(Key::Cap, "power cap", "%", 100., 1.),
            ))
    }

    /// What is set once for a model of projector: its diodes and its beam.
    fn details(&self) -> gpui::Div {
        // The three diodes are the table's columns.
        let diode = |label: &'static str, hue: f32| {
            div()
                .flex_none()
                .w(px(CELL))
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(gpui::hsla(hue / 360., 0.85, 0.6, 1.)),
                )
                .child(luma_ui::float::label(label))
        };
        let ttl = self.settings.is_on(Key::Ttl);
        let modulation = luma_ui::float::segmented().children(
            [("Analog", false), ("On or off (TTL)", true)].map(|(label, to)| {
                let set = self.change(true);
                luma_ui::float::segment(label, ttl == to, label)
                    .id(SharedString::from(self.id(label)))
                    .on_click(move |_, _, cx| set(vec![(Key::Ttl, f64::from(u8::from(to)))], cx))
                    .agent_node(Role::Toggle, format!("{} {label}", self.name))
                    .agent_focused(ttl == to)
            }),
        );
        let lumens = format!("{:.0} lumens of white", self.settings.lumens());
        let diodes = group("Diodes")
            .child(
                line("")
                    .child(diode("Red", 0.))
                    .child(diode("Green", 130.))
                    .child(diode("Blue", 215.)),
            )
            .child(
                line("Power")
                    .child(self.number(Key::PowerRed, "red power", "W", 1., 0.05))
                    .child(self.number(Key::PowerGreen, "green power", "W", 1., 0.05))
                    .child(self.number(Key::PowerBlue, "blue power", "W", 1., 0.05)),
            )
            .child(
                line("Wavelength")
                    .child(self.number(Key::WavelengthRed, "red wavelength", "nm", 1., 1.))
                    .child(self.number(Key::WavelengthGreen, "green wavelength", "nm", 1., 1.))
                    .child(self.number(Key::WavelengthBlue, "blue wavelength", "nm", 1., 1.)),
            )
            .child(
                line("Threshold")
                    .child(self.number(Key::FloorRed, "red threshold", "%", 100., 1.))
                    .child(self.number(Key::FloorGreen, "green threshold", "%", 100., 1.))
                    .child(self.number(Key::FloorBlue, "blue threshold", "%", 100., 1.)),
            )
            .child(line("Brightness").child(
                quiet(lumens.clone()).agent_node(Role::Text, format!("{}: {lumens}", self.name)),
            ))
            .child(line("Modulation").child(modulation));
        let beam = group("Beam")
            .child(line("Width").child(self.number(Key::Beam, "beam width", "mm", 1., 0.5)))
            .child(line("Divergence").child(self.number(
                Key::Divergence,
                "divergence",
                "mrad",
                1.,
                0.1,
            )))
            .child(line("Colour late by").child(self.number(
                Key::ColourDelay,
                "colour delay",
                "pts",
                1.,
                1.,
            )));
        div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap_x(px(36.0))
            .gap_y(px(24.0))
            .child(diodes)
            .child(beam)
    }
}

/// What the venue's projectors are: set once, for all of them.
fn kind_card(state: &Patch, lasers: &Lasers, app: &Entity<Luma>) -> AnyElement {
    let kind = Kind {
        venue: state.venue_id.clone(),
        projector: None,
        name: EVERY.into(),
        settings: lasers.rig.shared,
        app: app.clone(),
    };
    let open = state.lasers.details;
    let (opened, venue) = (app.clone(), state.venue_id.clone());
    let following = lasers.rig.projectors.iter().filter(|p| !p.own).count();
    let saying = format!(
        "What {} here {}. One that differs has its own, under its Setup.",
        if following == lasers.rig.projectors.len() {
            "every projector".to_owned()
        } else {
            format!(
                "{following} of {}",
                crate::patch::plural(lasers.rig.projectors.len(), "projector")
            )
        },
        if following == 1 { "is" } else { "are" },
    );
    card()
        .child(
            head(EVERY)
                .child(
                    quiet(saying.clone())
                        .flex_1()
                        .agent_node(Role::Text, saying),
                )
                .child(
                    disclosure("Details", open, "laser-details".into())
                        .on_click(move |_, _, cx| {
                            let venue = venue.clone();
                            opened.update(cx, |this, cx| {
                                this.toggle_laser_panel(venue, |state| state.details = !open, cx);
                            });
                        })
                        .agent_node(Role::Button, "Projector details")
                        .agent_focused(open),
                ),
        )
        .child(div().px(px(PAD)).pb(px(PAD)).child(kind.row()))
        .when(open, |card| {
            card.child(
                div()
                    .p(px(PAD))
                    .border_t_1()
                    .border_color(luma_ui::glass::hairline(0.08))
                    .child(kind.details()),
            )
        })
        .agent_node(Role::Row, "Projector kind")
        .into_any_element()
}

// ---------------------------------------------------------------------------
// A projector
// ---------------------------------------------------------------------------

/// One projector: what plays it, and under "Setup" what is its own.
fn projector_card(
    state: &Patch,
    lasers: &Lasers,
    projector: &Projector,
    app: &Entity<Luma>,
) -> AnyElement {
    let name = state.projector_name(&projector.id);
    let id = projector.id.clone();
    let venue = state.venue_id.clone();
    let open = state.lasers.more.as_deref() == Some(id.as_str());
    let testing = state.lasers.testing.as_deref() == Some(id.as_str());
    let (reading, dot) = link(lasers, projector);
    let (tested, opened) = (app.clone(), app.clone());
    let (venue_test, venue_more) = (venue.clone(), venue.clone());
    let (id_test, id_more) = (id.clone(), id.clone());
    let test = if testing { "Stop test" } else { "Test" };
    let top = head(name.clone())
        .child(
            status(reading.clone(), dot)
                .flex_1()
                .agent_node(Role::Text, format!("{name}: {reading}")),
        )
        .when(projector.own, |head| {
            head.child(luma_ui::float::tag("Own kind"))
        })
        .child(dac(state, lasers, projector, app))
        .child(
            action(test, format!("laser-test-{id}"))
                .on_click(move |_, _, cx| {
                    let venue = venue_test.clone();
                    let projector = (!testing).then(|| id_test.clone());
                    tested.update(cx, |this, cx| this.test_laser(venue, projector, cx));
                })
                .agent_node(Role::Button, format!("{test} {name}"))
                .agent_focused(testing),
        )
        .child(
            disclosure("Setup", open, format!("laser-more-{id}"))
                .on_click(move |_, _, cx| {
                    let (venue, id) = (venue_more.clone(), id_more.clone());
                    opened.update(cx, |this, cx| {
                        this.toggle_laser_panel(
                            venue,
                            |state| state.more = (state.more.as_ref() != Some(&id)).then_some(id),
                            cx,
                        );
                    });
                })
                .agent_node(Role::Button, format!("More about {name}"))
                .agent_focused(open),
        );
    card()
        .child(top)
        .when(open, |card| {
            card.child(setup(state, lasers, projector, app))
        })
        .agent_node(Role::Row, format!("Projector {name}"))
        .into_any_element()
}

/// How the projector's DAC is doing, and the dot that goes with it.
fn link(lasers: &Lasers, projector: &Projector) -> (String, gpui::Hsla) {
    let idle = ladder::foreground_alpha(0.3);
    let (ok, warn) = (ladder::status_ok().into(), ladder::status_warn().into());
    let Some(route) = lasers
        .routes
        .iter()
        .find(|route| route.fixture_id == projector.id)
    else {
        return ("No DAC: it draws on screen only".into(), idle);
    };
    let link = lasers
        .output
        .as_ref()
        .and_then(|output| output.links.get(&projector.id));
    match link {
        Some(LaserLink::Playing(scans)) if *scans < FLICKER_RATE => (
            format!("{scans:.0} scans a second: too much to draw steadily"),
            warn,
        ),
        Some(LaserLink::Playing(scans)) => (format!("Playing · {scans:.0} scans a second"), ok),
        Some(LaserLink::Idle) => ("Connected".into(), ok),
        Some(LaserLink::Fault(why)) => (why.clone(), warn),
        Some(LaserLink::Searching) | None => (format!("{} not found", device_name(route)), warn),
    }
}

fn device_name(route: &LaserRoute) -> String {
    route
        .device_name
        .clone()
        .unwrap_or_else(|| route.device_id.clone())
}

/// The DAC that plays the projector: a menu of the ones in reach that no
/// other projector has.
fn dac(state: &Patch, lasers: &Lasers, projector: &Projector, app: &Entity<Luma>) -> AnyElement {
    const NONE: &str = "None";
    let name = state.projector_name(&projector.id);
    let route = lasers
        .routes
        .iter()
        .find(|route| route.fixture_id == projector.id);
    let shown = route.map_or_else(|| NONE.to_owned(), device_name);
    let toggle = {
        let (app, venue, id) = (app.clone(), state.venue_id.clone(), projector.id.clone());
        move |cx: &mut gpui::App| {
            let (venue, id) = (venue.clone(), id.clone());
            app.update(cx, |this, cx| {
                this.toggle_laser_panel(
                    venue,
                    |state| state.binding = (state.binding.as_ref() != Some(&id)).then_some(id),
                    cx,
                );
            });
        }
    };
    let open = state.lasers.binding.as_deref() == Some(projector.id.as_str());
    let menu = open.then(|| {
        // `to` is the DAC a row binds, `None` for the row that is the one
        // already bound: choosing it changes nothing.
        let choice = |label: String, chosen: bool, to: Option<Option<LaserDevice>>| {
            let (picked, venue, id) = (app.clone(), state.venue_id.clone(), projector.id.clone());
            let close = toggle.clone();
            luma_ui::luma_select_item(&label, RowState::of(chosen, false))
                .id(SharedString::from(format!(
                    "laser-bind-{}-{label}",
                    projector.id
                )))
                .on_click(move |_, _, cx| match to.clone() {
                    Some(device) => {
                        let (venue, id) = (venue.clone(), id.clone());
                        picked.update(cx, |this, cx| this.bind_laser(venue, id, device, cx));
                    }
                    None => close(cx),
                })
                .agent_node(Role::Button, format!("{name}: {label}"))
                .into_any_element()
        };
        let free = lasers.devices.iter().filter(|device| {
            !lasers
                .routes
                .iter()
                .any(|route| route.device_kind == device.kind && route.device_id == device.id)
        });
        let mut rows = vec![choice(NONE.into(), route.is_none(), Some(None))];
        rows.extend(route.map(|route| choice(device_name(route), true, None)));
        let before = rows.len();
        rows.extend(free.map(|device| {
            choice(
                format!("{} · {}", device.name, device.address),
                false,
                Some(Some(device.clone())),
            )
        }));
        if rows.len() == before {
            rows.push(
                luma_ui::float::empty_row("No other DAC found on the network yet.")
                    .into_any_element(),
            );
        }
        let close = toggle.clone();
        luma_ui::float::anchored_below(
            format!("laser-dac-menu-{}", projector.id),
            luma_ui::CONTROL_HEIGHT,
            Dismiss::on_press_out(move |_, cx| close(cx)),
            luma_ui::float::popover_card()
                .min_w(px(220.))
                .children(rows)
                .into_any_element(),
        )
    });
    luma_ui::float::picker_chip(&shown, &[shown.as_str()])
        .id(SharedString::from(format!("laser-dac-{}", projector.id)))
        .on_click(move |_, _, cx| toggle(cx))
        .children(menu)
        .agent_node(Role::Button, format!("Choose a DAC for {name}"))
        .agent_focused(open)
        .into_any_element()
}

/// What is one projector's own: how its picture is sent to its DAC for the
/// way it hangs, where it may send light, and, when it is not the kind of
/// projector the rest are, its kind.
fn setup(state: &Patch, lasers: &Lasers, projector: &Projector, app: &Entity<Luma>) -> AnyElement {
    let name = state.projector_name(&projector.id);
    let kind = Kind {
        venue: state.venue_id.clone(),
        projector: Some(projector.id.clone()),
        name: name.clone(),
        settings: projector.settings,
        app: app.clone(),
    };
    // Across and up are the table's columns.
    let axis = |label: &'static str| luma_ui::float::label(label).w(px(CELL));
    let output = group("DAC output")
        .child(line("").child(axis("Across")).child(axis("Up")))
        .child(
            line("Mirror")
                .child(kind.switch(Key::FlipX, "", "Mirror across"))
                .child(kind.switch(Key::FlipY, "", "Mirror up")),
        )
        .child(
            line("Size")
                .child(kind.number(Key::SizeX, "output width", "%", 100., 1.))
                .child(kind.number(Key::SizeY, "output height", "%", 100., 1.)),
        )
        .child(
            line("Offset")
                .child(kind.number(Key::OffsetX, "output offset across", "%", 100., 1.))
                .child(kind.number(Key::OffsetY, "output offset up", "%", 100., 1.)),
        )
        .child(line("Swap axes").child(kind.switch(Key::Swap, "", "Swap axes")))
        .child(
            quiet(
                "For the way this box hangs and its DAC is wired: upside down, behind a \
                 screen, or with a cable that crosses the axes. It changes what the DAC is \
                 sent, never the picture here.",
            )
            .max_w(px(W_NAME + 2.0 * CELL + 2.0 * GAP)),
        );

    let own = projector.own;
    let (owned, venue, id) = (app.clone(), state.venue_id.clone(), projector.id.clone());
    let own_switch = div()
        .id(SharedString::from(format!("laser-own-{id}")))
        .flex()
        .items_center()
        .gap(px(8.0))
        .cursor_pointer()
        .child(luma_ui::float::switch(if own { 1.0 } else { 0.0 }))
        .child(
            div()
                .text_size(px(12.0))
                .child("A different kind of projector from the rest"),
        )
        .on_click(move |_, _, cx| {
            let (venue, id) = (venue.clone(), id.clone());
            owned.update(cx, |this, cx| this.set_laser_own(venue, id, !own, cx));
        })
        .agent_node(Role::Toggle, format!("{name} has its own kind"))
        .agent_focused(own);

    div()
        .border_t_1()
        .border_color(luma_ui::glass::hairline(0.08))
        .p(px(PAD))
        .flex()
        .flex_col()
        .gap(px(20.0))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_x(px(36.0))
                .gap_y(px(24.0))
                .child(output)
                .child(limit(state, lasers, projector, app)),
        )
        .child(own_switch)
        .when(own, |setup| setup.child(kind.row()).child(kind.details()))
        .into_any_element()
}

/// Where in the square `at` is, as galvo coordinates: −1 to 1 across and up.
fn galvo_at(bounds: Bounds<Pixels>, at: Point<Pixels>) -> [f64; 2] {
    let along = |at: Pixels, origin: Pixels, size: Pixels| {
        (f64::from(f32::from(at - origin) / f32::from(size).max(1.0)) * 2. - 1.).clamp(-1., 1.)
    };
    [
        along(at.x, bounds.origin.x, bounds.size.width),
        -along(at.y, bounds.origin.y, bounds.size.height),
    ]
}

/// Where the projector may send light: a polygon in its own scan, drawn in
/// a square that is the scan, with the venue's window as it falls in it.
fn limit(state: &Patch, lasers: &Lasers, projector: &Projector, app: &Entity<Luma>) -> AnyElement {
    let name = state.projector_name(&projector.id);
    let (venue, id) = (state.venue_id.clone(), projector.id.clone());
    let corners = projector.limit.clone();
    // The window's edge as this projector sees it, in its scan and past its
    // edge: the square cuts off what is outside.
    let window = lasers.rig.window();
    let edge: Vec<[f64; 2]> = [
        ([-1., -1.], [1., -1.]),
        ([1., -1.], [1., 1.]),
        ([1., 1.], [-1., 1.]),
        ([-1., 1.], [-1., -1.]),
    ]
    .into_iter()
    .flat_map(|(from, to): ([f64; 2], [f64; 2])| {
        (0..=8).map(move |k| {
            let t = f64::from(k) / 8.;
            [
                from[0] + (to[0] - from[0]) * t,
                from[1] + (to[1] - from[1]) * t,
            ]
        })
    })
    .filter_map(|[x, y]| projector.galvo_of(window.direction(x, y)))
    .collect();

    let measured = state.lasers.limit_box.clone();
    let drawn = corners.clone();
    let picture = canvas(
        move |bounds, _, _| {
            *measured.borrow_mut() = Some(bounds);
        },
        move |bounds, (), window, _| {
            let at = |[x, y]: [f64; 2]| {
                #[allow(clippy::cast_possible_truncation)]
                point(
                    bounds.origin.x + bounds.size.width * ((x as f32 + 1.) / 2.),
                    bounds.origin.y + bounds.size.height * ((1. - y as f32) / 2.),
                )
            };
            let mut stroke = |points: &[[f64; 2]], closed: bool, color: gpui::Hsla| {
                let mut path = PathBuilder::stroke(px(1.5));
                let Some(first) = points.first() else {
                    return;
                };
                path.move_to(at(*first));
                for corner in &points[1..] {
                    path.line_to(at(*corner));
                }
                if closed {
                    path.line_to(at(*first));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            stroke(&edge, true, ladder::foreground_alpha(0.28));
            if let Some(corners) = &drawn {
                stroke(corners, true, ladder::accent().into());
            }
        },
    )
    .absolute()
    .size_full();

    let (moved, dropped, dropped_out) = (app.clone(), app.clone(), app.clone());
    let (venue_move, venue_up, venue_out) = (venue.clone(), venue.clone(), venue.clone());
    let dragging = state.lasers.limit_drag.is_some();
    let mut square = div()
        .id(SharedString::from(format!("laser-limit-{id}")))
        .relative()
        .flex_none()
        .size(px(LIMIT_BOX))
        .rounded(px(luma_ui::radius::ROW))
        .bg(luma_ui::glass::ink(0.06))
        .overflow_hidden()
        .child(picture)
        .on_mouse_move(move |event, _, cx| {
            if dragging {
                let venue = venue_move.clone();
                moved.update(cx, |this, cx| {
                    this.drag_laser_limit(venue, event.position, cx)
                });
            }
        })
        .on_mouse_up(MouseButton::Left, move |_, _, cx| {
            let venue = venue_up.clone();
            dropped.update(cx, |this, cx| this.drop_laser_limit(venue, cx));
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
            let venue = venue_out.clone();
            dropped_out.update(cx, |this, cx| this.drop_laser_limit(venue, cx));
        });
    for (index, [x, y]) in corners.iter().flatten().copied().enumerate() {
        let (grabbed, removed) = (app.clone(), app.clone());
        let (venue_grab, venue_remove) = (venue.clone(), venue.clone());
        let (id_grab, id_remove) = (id.clone(), id.clone());
        let without = corners.clone().map(|mut corners| {
            corners.remove(index);
            corners
        });
        #[allow(clippy::cast_possible_truncation)]
        let (left, top) = ((x as f32 + 1.) / 2., (1. - y as f32) / 2.);
        square = square.child(
            div()
                .id(SharedString::from(format!("laser-limit-{id}-{index}")))
                .absolute()
                .left(relative(left))
                .top(relative(top))
                .ml(px(-HANDLE / 2.))
                .mt(px(-HANDLE / 2.))
                .size(px(HANDLE))
                .rounded_full()
                .bg(ladder::accent())
                .cursor_grab()
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    let (venue, id) = (venue_grab.clone(), id_grab.clone());
                    grabbed.update(cx, |this, cx| {
                        this.toggle_laser_panel(
                            venue,
                            |state| state.limit_drag = Some((id, index)),
                            cx,
                        );
                    });
                })
                // A limit keeps at least three corners.
                .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                    let Some(without) = without.clone().filter(|corners| corners.len() >= 3) else {
                        return;
                    };
                    let (venue, id) = (venue_remove.clone(), id_remove.clone());
                    removed.update(cx, |this, cx| {
                        this.set_laser_limit(venue, id, Some(without), cx);
                    });
                })
                .agent_node(
                    Role::Button,
                    format!("{name} limit corner {} at {x:.2}, {y:.2}", index + 1),
                ),
        );
    }

    let act = |label: &'static str, limit: Option<Vec<[f64; 2]>>| {
        let (app, venue, id) = (app.clone(), venue.clone(), id.clone());
        action(label, format!("laser-limit-{id}-{label}"))
            .on_click(move |_, _, cx| {
                let (venue, id, limit) = (venue.clone(), id.clone(), limit.clone());
                app.update(cx, |this, cx| this.set_laser_limit(venue, id, limit, cx));
            })
            .agent_node(Role::Button, format!("{label}: {name}"))
            .into_any_element()
    };
    // Lifting a limit lets light go where it was kept from: asked first.
    let lift = {
        let (app, venue, id, name) = (app.clone(), venue.clone(), id.clone(), name.clone());
        let label = format!("Remove the limit: {name}");
        action("Remove the limit", format!("laser-limit-{id}-remove"))
            .on_click(move |_, _, cx| {
                let confirm = Confirm {
                    title: "Remove the limit?".into(),
                    body: format!("{name} may then send light anywhere in its scan.").into(),
                    verb: "Remove the limit".into(),
                    action: Action::RemoveLaserLimit {
                        venue_id: venue.clone(),
                        projector_id: id.clone(),
                    },
                };
                app.update(cx, |this, cx| this.ask(confirm, cx));
            })
            .agent_node(Role::Button, label)
            .into_any_element()
    };
    let (saying, acts): (&str, Vec<AnyElement>) = match &corners {
        None => (
            "No limit: it may send light anywhere in its scan.",
            vec![act("Add a limit", Some(FIRST_LIMIT.to_vec()))],
        ),
        Some(corners) => {
            // A new corner goes in the middle of the longest side.
            let longest = (0..corners.len())
                .max_by(|a, b| {
                    let side = |i: usize| {
                        let (from, to) = (corners[i], corners[(i + 1) % corners.len()]);
                        (to[0] - from[0]).hypot(to[1] - from[1])
                    };
                    side(*a).total_cmp(&side(*b))
                })
                .unwrap_or(0);
            let (from, to) = (corners[longest], corners[(longest + 1) % corners.len()]);
            let mut with = corners.clone();
            with.insert(
                longest + 1,
                [(from[0] + to[0]) / 2., (from[1] + to[1]) / 2.],
            );
            (
                "Light outside the outline is blanked. Drag a corner; right-click removes one.",
                vec![act("Add a corner", Some(with)), lift],
            )
        }
    };
    group("Limit")
        .w(px(LIMIT_BOX))
        .child(square)
        .child(quiet(saying).agent_node(Role::Text, format!("{name}: {saying}")))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(div().w(px(14.0)).h(px(2.0)).bg(ladder::accent()))
                .child(quiet("Limit"))
                .child(
                    div()
                        .ml(px(8.0))
                        .w(px(14.0))
                        .h(px(2.0))
                        .bg(ladder::foreground_alpha(0.28)),
                )
                .child(quiet("The venue's window")),
        )
        .child(quiet("The square is everything this projector can scan."))
        .child(div().flex().flex_wrap().gap(px(8.0)).children(acts))
        .agent_node(Role::Card, format!("{name} limit"))
        .into_any_element()
}
