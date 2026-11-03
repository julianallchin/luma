//! The Perform tab: one venue's live show.
//!
//! Three things, top to bottom: the DJ source being followed and its decks,
//! then a fader for every live control of the venue. A control exists by
//! being named in a clip (`control("drop")`), so the faders are the venue's
//! clips' own inputs; moving one changes the lights at the next frame
//! without compiling anything. Under each fader, Learn binds the next MIDI or
//! OSC input heard to it, and each binding is a chip that cycles its mode.
//!
//! The backend owns everything live — the follow session, the control bank,
//! the input hub — so this tab only reads and asks. It polls while it is
//! open: the decks' positions and the last input heard change many times a
//! second and are nobody's event.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity, SharedString};
use luma_lib::controls::Control;
use luma_lib::dispatch::handlers::inputs::{Address, Binding, InputState, Mode};
use luma_lib::models::perform::{DeckMatch, PerformDeck, PerformSource, PerformState};
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::{float, glass, ladder};

use crate::shell::Body;
use crate::tabs::Target;
use crate::Luma;

/// How often the decks and inputs are read while the tab is open.
const POLL: Duration = Duration::from_millis(100);
/// The controls come from the venue's clips, which change rarely: read them
/// every this many polls.
const CONTROLS_EVERY: u64 = 10;
/// How long a fader the person moved keeps its own value over what a poll
/// read, so a read that left before the move cannot pull it back.
const HELD_FOR: Duration = Duration::from_millis(400);
const FADER_HEIGHT: f32 = 180.;

pub(crate) struct Perform {
    pub(crate) venue_id: String,
    pub(crate) chat: crate::agent::TabChat,
    controls: Vec<Control>,
    inputs: Option<InputState>,
    follow: Option<PerformState>,
    /// Faders moved here, with when, so a stale read does not snap them back.
    held: HashMap<String, Instant>,
    /// The control a Learn pressed here waits to bind.
    learning: Option<String>,
    error: Option<String>,
    polls: u64,
}

impl Luma {
    /// Open the selected venue's Perform tab, or bring it to the front.
    pub(crate) fn open_perform_tab(&mut self, thread: Option<&str>, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let venue_id = browser.venue_id().to_string();
        let target = Target::Perform {
            venue: venue_id.clone(),
        };
        if self.workspace.body(&target).is_some() {
            self.workspace.select(&target);
            cx.notify();
            return;
        }
        let chat = self.tab_chat(&target, thread, cx);
        let state = Perform {
            venue_id: venue_id.clone(),
            chat,
            controls: Vec::new(),
            inputs: None,
            follow: None,
            held: HashMap::new(),
            learning: None,
            error: None,
            polls: 0,
        };
        self.open_tab(target.clone(), move || Body::Perform(Box::new(state)), cx);
        self.poll_perform(target.clone(), cx);
        let opened = self.library.input_open(&venue_id);
        cx.spawn(async move |this, cx| {
            let opened = opened.await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.perform_mut(&target) {
                    match opened {
                        Ok(inputs) => state.inputs = Some(inputs),
                        Err(error) => state.error = Some(error.to_string()),
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn perform_mut(&mut self, target: &Target) -> Option<&mut Perform> {
        match self.parked.body_mut(&mut self.workspace, target) {
            Some(Body::Perform(state)) => Some(state),
            _ => None,
        }
    }

    /// Read the follow session, the inputs and (now and then) the controls,
    /// for as long as the tab is open.
    fn poll_perform(&mut self, target: Target, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            // The reads are made inside the update: the library is the app's,
            // and a tab that closed since the last poll ends the loop here.
            let Ok(Some((follow, inputs, controls))) = this.update(cx, |this, _| {
                let venue = this.perform_mut(&target).map(|state| {
                    state.polls += 1;
                    (state.polls, state.venue_id.clone())
                });
                venue.map(|(polls, venue)| {
                    (
                        this.library.perform_state(),
                        this.library.input_state(),
                        (polls % CONTROLS_EVERY == 1).then(|| this.library.control_list(&venue)),
                    )
                })
            }) else {
                return;
            };
            let follow = follow.await;
            let inputs = inputs.await;
            let controls = match controls {
                Some(controls) => Some(controls.await),
                None => None,
            };
            let landed = this.update(cx, |this, cx| {
                let Some(state) = this.perform_mut(&target) else {
                    return None;
                };
                state.follow = follow.ok();
                if let Ok(inputs) = inputs {
                    // Learning ended somewhere other than here: a binding
                    // landed, or it was cancelled.
                    if inputs.learning.is_none() {
                        state.learning = None;
                    }
                    state.inputs = Some(inputs);
                }
                match controls {
                    Some(Ok(controls)) => state.landed_controls(controls),
                    Some(Err(error)) => state.error = Some(error.to_string()),
                    None => {}
                }
                cx.notify();
                Some(this.library.debounce(POLL))
            });
            let Ok(Some(wait)) = landed else {
                return;
            };
            wait.await;
        })
        .detach();
    }

    /// The tab closed: stop waiting on a Learn it started. Following and the
    /// controls carry on; the show does not stop because a tab did.
    pub(crate) fn close_perform(&mut self, state: &Perform, cx: &mut Context<Self>) {
        if state.learning.is_some() {
            let cancel = self.library.input_cancel_learn();
            cx.background_spawn(async move {
                cancel.await.ok();
            })
            .detach();
        }
    }

    fn set_control(&mut self, target: &Target, name: &str, value: f32, cx: &mut Context<Self>) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        let value = f64::from(value.clamp(0., 1.));
        if let Some(control) = state.controls.iter_mut().find(|c| c.name == name) {
            control.value = value;
        }
        state.held.insert(name.to_owned(), Instant::now());
        let set = self.library.control_set(target.venue(), name, value);
        cx.notify();
        cx.background_spawn(async move {
            set.await.ok();
        })
        .detach();
    }

    fn learn_control(&mut self, target: &Target, name: &str, cx: &mut Context<Self>) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        if state.learning.as_deref() == Some(name) {
            state.learning = None;
            let cancel = self.library.input_cancel_learn();
            cx.notify();
            cx.background_spawn(async move {
                cancel.await.ok();
            })
            .detach();
            return;
        }
        state.learning = Some(name.to_owned());
        state.error = None;
        let learn = self.library.input_learn(target.venue(), name, Mode::Fader);
        let target = target.clone();
        let name = name.to_owned();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let learned = learn.await;
            this.update(cx, |this, cx| {
                let Some(state) = this.perform_mut(&target) else {
                    return;
                };
                if state.learning.as_deref() == Some(name.as_str()) {
                    state.learning = None;
                    if let Err(error) = learned {
                        state.error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn change_binding(
        &mut self,
        target: &Target,
        binding: &Binding,
        change: BindingChange,
        cx: &mut Context<Self>,
    ) {
        let venue = target.venue();
        let pending = match change {
            BindingChange::NextMode => self.library.input_update_binding(
                venue,
                &binding.id,
                next_mode(binding.mode),
                binding.invert,
            ),
            BindingChange::Invert => {
                self.library
                    .input_update_binding(venue, &binding.id, binding.mode, !binding.invert)
            }
            BindingChange::Remove => {
                let unbind = self.library.input_unbind(venue, &binding.id);
                return self.after_request(target.clone(), unbind, cx);
            }
        };
        self.after_request(target.clone(), pending, cx);
    }

    fn after_request(
        &mut self,
        target: Target,
        pending: impl std::future::Future<Output = Result<(), crate::LibraryError>> + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let done = pending.await;
            this.update(cx, |this, cx| {
                if let (Some(state), Err(error)) = (this.perform_mut(&target), done) {
                    state.error = Some(error.to_string());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn follow_source(
        &mut self,
        target: &Target,
        source: Option<PerformSource>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.perform_mut(target) else {
            return;
        };
        state.error = None;
        let target = target.clone();
        match source {
            Some(source) => {
                let start = self.library.perform_start(target.venue(), source);
                self.after_request(target, start, cx);
            }
            None => {
                let stop = self.library.perform_stop();
                self.after_request(target, stop, cx);
            }
        }
    }

    fn set_perform_latency(&mut self, latency_ms: f32, cx: &mut Context<Self>) {
        let set = self.library.perform_set_latency(latency_ms.round());
        cx.background_spawn(async move {
            set.await.ok();
        })
        .detach();
    }
}

impl Perform {
    fn landed_controls(&mut self, mut controls: Vec<Control>) {
        self.held.retain(|_, at| at.elapsed() < HELD_FOR);
        for control in &mut controls {
            if self.held.contains_key(&control.name) {
                if let Some(mine) = self.controls.iter().find(|c| c.name == control.name) {
                    control.value = mine.value;
                }
            }
        }
        self.controls = controls;
    }
}

#[derive(Clone, Copy)]
enum BindingChange {
    NextMode,
    Invert,
    Remove,
}

fn next_mode(mode: Mode) -> Mode {
    match mode {
        Mode::Fader => Mode::Momentary,
        Mode::Momentary => Mode::Toggle,
        Mode::Toggle => Mode::Fader,
    }
}

fn mode_word(mode: Mode) -> &'static str {
    match mode {
        Mode::Fader => "Fader",
        Mode::Momentary => "Hold",
        Mode::Toggle => "Toggle",
    }
}

/// An input as a person reads it: "CC 7 · ch 1", "Note 36 · ch 10",
/// "/glove/tilt 2".
fn address_label(address: &Address) -> String {
    match address {
        Address::MidiCc { channel, number } => format!("CC {number} · ch {}", channel + 1),
        Address::MidiNote { channel, number } => format!("Note {number} · ch {}", channel + 1),
        Address::Osc { address, arg: 0 } => address.clone(),
        Address::Osc { address, arg } => format!("{address} {arg}"),
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub(crate) fn perform(state: &Perform, app: &Entity<Luma>) -> AnyElement {
    let target = Target::Perform {
        venue: state.venue_id.clone(),
    };
    div()
        .id("perform")
        .size_full()
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .child(band(state, &target, app))
        .children(
            state
                .error
                .clone()
                .map(|error| div().px(px(20.)).pt(px(12.)).child(float::error_row(error))),
        )
        .child(decks(state))
        .child(faders(state, &target, app))
        .into_any_element()
}

/// Title, the source chooser, its status and the latency.
fn band(state: &Perform, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    let follow = state.follow.as_ref();
    let source = follow.and_then(|follow| follow.source.as_ref());
    let latency = follow.map_or(0., |follow| follow.latency_ms);
    let choices: [(&str, Option<PerformSource>); 4] = [
        ("Off", None),
        ("Rekordbox", Some(PerformSource::RkbxLink { address: None })),
        (
            "Pro DJ Link",
            Some(PerformSource::ProDjLink { device_num: 5 }),
        ),
        ("StageLinQ", Some(PerformSource::StageLinq)),
    ];
    let chooser = choices
        .into_iter()
        .fold(float::segmented(), |track, (label, choice)| {
            let chosen = match (&choice, source) {
                (None, None) => true,
                (Some(choice), Some(source)) => {
                    std::mem::discriminant(choice) == std::mem::discriminant(source)
                }
                _ => false,
            };
            let app = app.clone();
            let target = target.clone();
            let key = format!("perform-source-{label}");
            track.child(
                float::segment(label, chosen, key.clone())
                    .id(SharedString::from(key))
                    .on_click(move |_, _, cx| {
                        let choice = choice.clone();
                        app.update(cx, |this, cx| this.follow_source(&target, choice, cx));
                    })
                    .agent_node(Role::Toggle, label)
                    .agent_focused(chosen),
            )
        });
    let latency_app = app.clone();
    div()
        .flex_shrink_0()
        .px(px(20.))
        .py(px(14.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .border_b_1()
        .border_color(glass::hairline(0.07))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .mr(px(8.))
                        .text_size(px(15.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(ladder::foreground())
                        .child("Perform"),
                )
                .child(chooser)
                .child(div().flex_1())
                .child(luma_ui::caption("Latency (ms)"))
                .child(
                    luma_ui::luma_slider(
                        "perform-latency",
                        latency,
                        -200.,
                        200.,
                        140.,
                        move |value, _, cx| {
                            latency_app.update(cx, |this, cx| this.set_perform_latency(value, cx));
                        },
                    )
                    .agent_node(Role::Slider, "Latency"),
                ),
        )
        .child(status_line(follow, state.inputs.as_ref()))
}

/// What the source and the inputs are doing, in one line of small type.
fn status_line(follow: Option<&PerformState>, inputs: Option<&InputState>) -> impl IntoElement {
    let mut parts: Vec<String> = Vec::new();
    match follow {
        Some(follow) if follow.source.is_some() => {
            parts.push(if follow.detail.is_empty() {
                if follow.connected {
                    "Connected"
                } else {
                    "Connecting…"
                }
                .to_string()
            } else {
                follow.detail.clone()
            });
            if let Some(ms) = follow.last_heard_ms {
                parts.push(format!("heard {:.1} s ago", ms as f64 / 1000.));
            }
        }
        _ => parts.push("Not following a DJ".into()),
    }
    if let Some(inputs) = inputs {
        parts.push(match inputs.midi_ports.as_slice() {
            [] => "No MIDI devices".into(),
            ports => format!("MIDI: {}", ports.join(", ")),
        });
        parts.push(format!("OSC: {}", inputs.osc));
        if let Some(last) = &inputs.last {
            if last.ms_ago < 5_000 {
                parts.push(format!(
                    "Last input {} = {:.2}",
                    address_label(&last.address),
                    last.value
                ));
            }
        }
    }
    let line = parts.join("  ·  ");
    let problem = follow.and_then(|follow| follow.problem.clone());
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(11.5))
                .text_color(ladder::foreground_alpha(0.55))
                .child(line.clone())
                .agent_node(Role::Text, line),
        )
        .children(problem.map(|problem| {
            div()
                .text_size(px(11.5))
                .text_color(ladder::danger())
                .child(problem.clone())
                .agent_node(Role::Text, problem)
        }))
}

fn decks(state: &Perform) -> impl IntoElement {
    let decks = state
        .follow
        .as_ref()
        .map(|follow| follow.decks.as_slice())
        .unwrap_or_default();
    div()
        .flex_shrink_0()
        .px(px(20.))
        .pt(px(16.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(float::section_heading("Decks"))
        .child(if decks.is_empty() {
            div()
                .text_size(px(12.))
                .text_color(ladder::foreground_alpha(0.45))
                .child("No decks yet. Pick a source above and load a track.")
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_wrap()
                .gap(px(10.))
                .children(decks.iter().map(deck))
                .into_any_element()
        })
}

fn deck(deck: &PerformDeck) -> impl IntoElement {
    let (status, live) = match &deck.track {
        DeckMatch::Unknown => ("Nothing loaded".to_string(), false),
        DeckMatch::Matching => ("Matching…".to_string(), false),
        DeckMatch::Matched {
            has_score: true, ..
        } => ("Driving the lights".to_string(), true),
        DeckMatch::Matched { .. } => ("No score in this venue".to_string(), false),
        DeckMatch::Unmatched => ("Not in your library".to_string(), false),
        DeckMatch::Failed { message } => (message.clone(), false),
    };
    let title = if deck.title.is_empty() {
        "—".to_string()
    } else {
        deck.title.clone()
    };
    let seconds = deck.position.max(0.) as u64;
    let readout = format!(
        "{}  {:>2}:{:02}  {:.1} BPM",
        if deck.playing { "▶" } else { "❚❚" },
        seconds / 60,
        seconds % 60,
        deck.bpm
    );
    let name = format!("Deck {}", deck.id);
    div()
        .w(px(240.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .rounded(px(luma_ui::radius::ROW))
        .bg(glass::card_bg())
        .border_1()
        .border_color(if live {
            gpui::Hsla::from(ladder::primary())
        } else {
            glass::hairline(0.08)
        })
        .child(
            div()
                .text_size(px(11.))
                .text_color(ladder::foreground_alpha(0.5))
                .child(name.clone()),
        )
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(ladder::foreground())
                .truncate()
                .child(title.clone()),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(ladder::foreground_alpha(0.6))
                .truncate()
                .child(deck.artist.clone()),
        )
        .child(
            div()
                .font_family(luma_ui::fonts::MONO)
                .text_size(px(11.))
                .text_color(ladder::foreground_alpha(0.8))
                .child(readout.clone()),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(if live {
                    ladder::foreground_alpha(0.9)
                } else {
                    ladder::foreground_alpha(0.5)
                })
                .child(status.clone()),
        )
        .agent_node(
            Role::Card,
            format!("{name}: {title} · {readout} · {status}"),
        )
}

fn faders(state: &Perform, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    let bindings = state
        .inputs
        .as_ref()
        .map(|inputs| inputs.bindings.as_slice())
        .unwrap_or_default();
    div()
        .flex_shrink_0()
        .px(px(20.))
        .py(px(16.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(float::section_heading("Controls"))
        .child(if state.controls.is_empty() {
            div()
                .text_size(px(12.))
                .text_color(ladder::foreground_alpha(0.45))
                .child(
                    "No controls yet. Add a \u{201c}Live control\u{201d} node to a clip, \
                     and its fader appears here.",
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_wrap()
                .gap(px(14.))
                .children(state.controls.iter().map(|control| {
                    let mine: Vec<&Binding> = bindings
                        .iter()
                        .filter(|b| b.control == control.name)
                        .collect();
                    let learning = state.learning.as_deref() == Some(control.name.as_str());
                    strip(control, &mine, learning, target, app)
                }))
                .into_any_element()
        })
}

/// One control: its name, its fader, its value, Learn, and its bindings.
fn strip(
    control: &Control,
    bindings: &[&Binding],
    learning: bool,
    target: &Target,
    app: &Entity<Luma>,
) -> impl IntoElement {
    let name = control.name.clone();
    let moved = (app.clone(), target.clone(), name.clone());
    let learn = (app.clone(), target.clone(), name.clone());
    let learn_key = format!("perform-learn-{name}");
    div()
        .w(px(96.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .child(
            div()
                .w_full()
                .text_center()
                .truncate()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(ladder::foreground())
                .child(name.clone()),
        )
        .child(
            luma_ui::luma_fader(
                SharedString::from(format!("perform-fader-{name}")),
                control.value as f32,
                FADER_HEIGHT,
                move |value, _, cx| {
                    let (app, target, name) = &moved;
                    app.update(cx, |this, cx| this.set_control(target, name, value, cx));
                },
            )
            .agent_node(Role::Slider, name.clone()),
        )
        .child(
            div()
                .font_family(luma_ui::fonts::MONO)
                .text_size(px(11.))
                .text_color(ladder::foreground_alpha(0.7))
                .child(format!("{:.0}%", control.value * 100.)),
        )
        .child(
            float::btn(
                if learning { "Move it…" } else { "Learn" },
                learn_key.clone(),
            )
            .id(SharedString::from(learn_key))
            .when(learning, |btn| btn.bg(glass::card_selected_bg()))
            .on_click(move |_, _, cx| {
                let (app, target, name) = &learn;
                app.update(cx, |this, cx| this.learn_control(target, name, cx));
            })
            .agent_node(Role::Button, format!("Learn {name}"))
            .agent_focused(learning),
        )
        .children(
            bindings
                .iter()
                .map(|binding| binding_chip(binding, target, app)),
        )
}

/// A binding: its input and mode (click to cycle the mode), invert, remove.
fn binding_chip(binding: &Binding, target: &Target, app: &Entity<Luma>) -> impl IntoElement {
    let label = address_label(&binding.address);
    let on = |change: BindingChange| {
        let (app, target, binding) = (app.clone(), target.clone(), binding.clone());
        move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
            app.update(cx, |this, cx| {
                this.change_binding(&target, &binding, change, cx)
            });
        }
    };
    let id = &binding.id;
    let small = |el: gpui::Div| {
        el.px(px(6.))
            .h(px(20.))
            .flex()
            .items_center()
            .rounded(px(luma_ui::radius::CONTROL))
            .text_size(px(10.5))
            .cursor_pointer()
            .hover(|style| style.bg(glass::glass_hover()))
    };
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(2.))
        .p(px(4.))
        .rounded(px(luma_ui::radius::ROW))
        .bg(glass::ink(0.04))
        .child(
            div()
                .w_full()
                .text_center()
                .truncate()
                .text_size(px(10.5))
                .text_color(ladder::foreground_alpha(0.8))
                .child(label.clone()),
        )
        .child(
            div()
                .flex()
                .justify_between()
                .child(
                    small(div())
                        .id(SharedString::from(format!("binding-mode-{id}")))
                        .text_color(ladder::foreground_alpha(0.75))
                        .child(mode_word(binding.mode))
                        .on_click(on(BindingChange::NextMode))
                        .agent_node(
                            Role::Button,
                            format!("{label} mode {}", mode_word(binding.mode)),
                        ),
                )
                .child(
                    small(div())
                        .id(SharedString::from(format!("binding-invert-{id}")))
                        .text_color(ladder::foreground_alpha(if binding.invert {
                            1.0
                        } else {
                            0.4
                        }))
                        .when(binding.invert, |el| el.bg(glass::card_selected_bg()))
                        .child("Inv")
                        .on_click(on(BindingChange::Invert))
                        .agent_node(Role::Toggle, format!("{label} invert"))
                        .agent_focused(binding.invert),
                )
                .child(
                    small(div())
                        .id(SharedString::from(format!("binding-remove-{id}")))
                        .text_color(ladder::foreground_alpha(0.55))
                        .child("✕")
                        .on_click(on(BindingChange::Remove))
                        .agent_node(Role::Button, format!("Remove {label}")),
                ),
        )
}
