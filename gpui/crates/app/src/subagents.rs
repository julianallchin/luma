//! What this thread delegated, and what each child said.
//!
//! # Two routes, one card
//!
//! The list is an inventory: every subagent this thread has heard from, running
//! and finished, named by the label its parent wrote for it and by what it is
//! doing right now. Picking one *morphs* the same card into that child's
//! conversation. It is a morph rather than a second dialog because it is the
//! same object being read at two depths — the frame stays, the content travels
//! — which is exactly what [`MorphDialog`] is for, and what the add-tracks
//! palette already does.
//!
//! # The child's transcript is not rendered here
//!
//! A subagent's messages are rows in a real `agent_threads` row, so reading one
//! is [`AgentChat::reader`] pointed at its id — the same transcript, the same
//! reducer, the same virtualized row stack the shell's centre uses, with the
//! header and composer left unbuilt. There is deliberately no second renderer:
//! a transcript view that only the dialog could draw would be the second
//! transcript store this whole design exists to avoid, one layer up.
//!
//! # Where the list comes from
//!
//! [`luma_chat::AgentChat::subagents`], which is live snapshot state and is
//! never persisted. So the dialog answers "what is this turn doing", and a
//! child whose delegation happened in an earlier session is reached from its
//! *chip* instead — the chip is durable, and clicking one opens this dialog
//! straight on that child's thread.

use std::collections::HashMap;

use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, FocusHandle, FontWeight, KeyDownEvent,
    SharedString, Window,
};
use gpui_component::IconName;
use luma_chat::AgentChat;
use luma_lib::agent::subagent::{SubagentPhase, SubagentSnapshot};
use luma_ui::dialog::morph::{self, ContentMode, MorphDialog, MorphSize, RouteDescriptor};
use luma_ui::float::{self, RowState};
use luma_ui::ladder;
use luma_ui::node::{AgentNode, Instrument, Role};

use crate::shell::Overlay;
use crate::Luma;

/// The card both routes wear. One size, for the reason the add-tracks palette
/// has one: a frame that resized between routes would spend the whole morph
/// animating the box instead of the content.
const CARD_SIZE: MorphSize = MorphSize::new(680.0, 520.0);
/// A subagent row: its label over what it is doing.
const ROW_HEIGHT: f32 = 46.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    List,
    /// One child, by thread id.
    Thread(SharedString),
}

impl Route {
    fn descriptor(&self) -> RouteDescriptor<Self> {
        RouteDescriptor::exact(self.clone(), CARD_SIZE.width, CARD_SIZE.height)
    }

    /// Where "back" goes from here — `None` at the root.
    fn parent(&self) -> Option<Self> {
        match self {
            Self::List => None,
            Self::Thread(_) => Some(Self::List),
        }
    }
}

pub(crate) struct Subagents {
    morph: MorphDialog<Route>,
    /// The children, mirrored out of the chat every tick.
    ///
    /// Mirrored rather than read through the chat entity at paint time so the
    /// row focus handles below can be rebuilt on the frame the set changes: a
    /// row that appeared without one would be a tab stop gpui settles onto the
    /// card instead, which reads as a broken ring.
    rows: Vec<SubagentSnapshot>,
    /// The read-only panel for the child being inspected, and whose it is.
    /// Built on the click that opens a child and dropped when the route leaves,
    /// so a closed dialog holds no transcript.
    reader: Option<(SharedString, Entity<AgentChat>)>,
    /// One handle per row, keyed by child thread id.
    row_focuses: HashMap<SharedString, FocusHandle>,
    back_focus: FocusHandle,
    close_focus: FocusHandle,
}

impl Subagents {
    fn new(rows: Vec<SubagentSnapshot>, cx: &mut Context<Luma>) -> Self {
        let mut state = Self {
            morph: MorphDialog::new(Route::List.descriptor(), CARD_SIZE),
            rows: Vec::new(),
            reader: None,
            row_focuses: HashMap::new(),
            back_focus: cx.focus_handle().tab_stop(true),
            close_focus: cx.focus_handle().tab_stop(true),
        };
        state.seat(rows, cx);
        state
    }

    /// Take the current snapshot list, minting a focus handle for any child
    /// that is new. Handles for children that vanished cannot exist — the list
    /// only grows within a session — so nothing is reaped.
    fn seat(&mut self, rows: Vec<SubagentSnapshot>, cx: &mut Context<Luma>) {
        for row in &rows {
            self.row_focuses
                .entry(SharedString::from(row.child_thread_id.clone()))
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
        self.rows = rows;
    }

    fn row(&self, thread: &str) -> Option<&SubagentSnapshot> {
        self.rows.iter().find(|row| row.child_thread_id == thread)
    }
}

impl Luma {
    /// Open the subagents dialog, optionally straight on one child.
    pub(crate) fn show_subagents(
        &mut self,
        child: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self
            .chat
            .as_ref()
            .map(|chat| chat.read(cx).subagents().to_vec())
            .unwrap_or_default();
        let agent = self.library.agent();
        let mut state = Subagents::new(rows, cx);
        if let Some(child) = child {
            open_child(&mut state, child, &agent, window, cx);
        }
        self.overlay.open(Overlay::Subagents(Box::new(state)));
        cx.notify();
    }

    fn open_subagent(&mut self, child: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        // Read the agent handle out first: `open_mut` below borrows `self`, and
        // reaching back through `cx.entity()` for it inside that borrow is a
        // read of an entity that is already being updated.
        let agent = self.library.agent();
        if let Some(Overlay::Subagents(state)) = self.overlay.open_mut() {
            open_child(state, child, &agent, window, cx);
            cx.notify();
        }
    }

    /// Go back to the list, and say whether there was anywhere to go back
    /// *from*.
    ///
    /// The answer is what makes Escape mean one thing: the shell binds Escape
    /// to [`Self::dismiss_overlay`] for every dialog, and this is consulted
    /// there rather than handled a second time in [`Self::subagents_key`] —
    /// two handlers on one key would step back *and* close.
    pub(crate) fn subagents_to_list(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(Overlay::Subagents(state)) = self.overlay.open_mut() else {
            return false;
        };
        if state.morph.target_key().parent().is_none() {
            return false;
        }
        // The reader dies with the route it belonged to: a dialog left on the
        // list must not be holding a transcript, and reopening the same child
        // re-reads it from the thread anyway.
        state.reader = None;
        state.morph.request(
            Route::List.descriptor(),
            std::time::Instant::now(),
            luma_ui::motion::reduced_motion(cx),
        );
        cx.notify();
        true
    }

    fn step_subagents(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Subagents(state)) = self.overlay.as_open() else {
            return;
        };
        if *state.morph.target_key() != Route::List || state.rows.is_empty() {
            return;
        }
        let focused = state
            .rows
            .iter()
            .position(|row| {
                state
                    .row_focuses
                    .get(row.child_thread_id.as_str())
                    .is_some_and(|handle| handle.is_focused(window))
            })
            .map_or(0, |at| {
                (at as isize + delta).rem_euclid(state.rows.len() as isize) as usize
            });
        let Some(handle) = state
            .rows
            .get(focused)
            .and_then(|row| state.row_focuses.get(row.child_thread_id.as_str()))
            .cloned()
        else {
            return;
        };
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Open whichever row the keyboard is on.
    fn open_focused_subagent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Subagents(state)) = self.overlay.as_open() else {
            return;
        };
        let child = state.rows.iter().find_map(|row| {
            state
                .row_focuses
                .get(row.child_thread_id.as_str())
                .filter(|handle| handle.is_focused(window))
                .map(|_| SharedString::from(row.child_thread_id.clone()))
        });
        if let Some(child) = child {
            self.open_subagent(child, window, cx);
        }
    }

    pub(crate) fn subagents_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.overlay.as_open(), Some(Overlay::Subagents(_))) {
            return;
        }
        match event.keystroke.key.as_str() {
            // Escape is deliberately absent: the shell's own binding reaches
            // `dismiss_overlay`, which consults `subagents_to_list` first.
            "left" | "backspace" => {
                if !self.subagents_to_list(cx) {
                    self.dismiss_overlay(cx);
                }
            }
            "up" => self.step_subagents(-1, window, cx),
            "down" => self.step_subagents(1, window, cx),
            "enter" | "right" => self.open_focused_subagent(window, cx),
            _ => {}
        }
    }
}

/// Point the card at one child: build its reader if it is not already the one
/// mounted, then ask the morph for the route.
///
/// A free function rather than a method on either side because it writes the
/// dialog's own state while the overlay that holds it is already borrowed from
/// `Luma`; the agent handle it needs is read out before that borrow and handed
/// in.
fn open_child(
    state: &mut Subagents,
    child: SharedString,
    agent: &luma_chat::Agent,
    window: &mut Window,
    cx: &mut Context<Luma>,
) {
    if state
        .reader
        .as_ref()
        .is_none_or(|(mounted, _)| mounted != &child)
    {
        let agent = agent.clone();
        let thread = child.clone();
        let reader = cx.new(|cx| AgentChat::reader(agent, &thread, window, cx));
        state.reader = Some((child.clone(), reader));
    }
    state.morph.request(
        Route::Thread(child).descriptor(),
        std::time::Instant::now(),
        luma_ui::motion::reduced_motion(cx),
    );
}

/// Advance the morph, and keep the list in step with what is running.
///
/// `rows` is handed in rather than read from the chat here: the caller already
/// holds `&mut Luma`, and reaching back through the entity for its own fields
/// inside that borrow is a read of an entity being updated.
pub(crate) fn tick(
    state: &mut Subagents,
    rows: Vec<SubagentSnapshot>,
    window: &mut Window,
    cx: &mut Context<Luma>,
) {
    if state.morph.tick(
        std::time::Instant::now(),
        luma_ui::motion::reduced_motion(cx),
    ) {
        window.request_animation_frame();
    }
    if rows != state.rows {
        state.seat(rows, cx);
        cx.notify();
    }
    // Where the keyboard should be: the first row on the list, the back arrow on
    // a thread — whichever the route makes the obvious next gesture. Every
    // dialog in the app claims focus this way.
    let held = state
        .row_focuses
        .values()
        .chain([&state.back_focus, &state.close_focus])
        .any(|handle| handle.is_focused(window));
    if held {
        return;
    }
    let wanted = match state.morph.target_key() {
        Route::List => state
            .rows
            .first()
            .and_then(|row| state.row_focuses.get(row.child_thread_id.as_str()))
            .unwrap_or(&state.close_focus),
        Route::Thread(_) => &state.back_focus,
    }
    .clone();
    window.focus(&wanted, cx);
}

pub(crate) fn render(
    state: &Subagents,
    app: &Entity<Luma>,
    window: &Window,
    _cx: &mut gpui::App,
) -> AnyElement {
    let sample = state.morph.sample(std::time::Instant::now());
    let app = app.clone();
    morph::card(&sample, "Subagents dialog", move |route, mode| {
        frame(state, route, mode, &app, window)
    })
}

/// Header band, body, footer legend — the shape both routes wear.
fn frame(
    state: &Subagents,
    route: &Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    let keys = app.clone();
    let mut card = div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_color(ladder::foreground());
    if mode == ContentMode::Interactive {
        // No `track_focus`: the dialog host already owns this card's focus
        // trap, and a second container would add a stop with no control on it.
        card = card.on_key_down(move |event, window, cx| {
            let event = event.clone();
            keys.update(cx, |this, cx| this.subagents_key(&event, window, cx));
        });
    }
    card.child(header(state, route, app, window))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(body(state, route, app, window)),
        )
        .child(footer(route))
        .into_any_element()
}

fn header(
    state: &Subagents,
    route: &Route,
    app: &Entity<Luma>,
    window: &Window,
) -> impl IntoElement {
    let close = app.clone();
    let back = app.clone();
    // The list carries running *and* finished children, so it is not titled by
    // a count — counting is the floating pill's job, and the pill counts only
    // what is still in flight.
    let title: SharedString = match route {
        Route::List => "Subagents".into(),
        Route::Thread(child) => state
            .row(child)
            .map_or_else(|| "Subagent".into(), |row| row.description.clone().into()),
    };
    float::header_band()
        .when_some(route.parent(), |band, _| {
            band.child(
                float::key_cap_pressable(float::key_cap())
                    .id("subagents-back")
                    .track_focus(&state.back_focus)
                    .tab_index(0)
                    .on_click(move |_, _, cx| {
                        back.update(cx, |this, cx| {
                            this.subagents_to_list(cx);
                        });
                    })
                    .child("←")
                    .agent_node(Role::Button, "Back")
                    .agent_focused(state.back_focus.is_focused(window)),
            )
        })
        // Seeded by the *call*, like every other sighting of this child: the
        // header's face and its row's face are the same subagent, and seeding
        // one of them by thread id would draw two.
        .when_some(
            match route {
                Route::Thread(child) => state.row(child).map(|row| row.call_id.clone()),
                Route::List => None,
            },
            |band, call| band.child(luma_chat::subagents::avatar(&call, AVATAR_LARGE)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(14.0))
                .font_weight(FontWeight::MEDIUM)
                .child(title.clone())
                .agent_node(Role::Text, title),
        )
        .child(
            float::key_cap_pressable(float::key_cap())
                .id("close-subagents")
                .track_focus(&state.close_focus)
                .tab_index(0)
                .on_click(move |_, _, cx| close.update(cx, |this, cx| this.dismiss_overlay(cx)))
                .child("esc")
                .agent_node(Role::Button, "Close")
                .agent_focused(state.close_focus.is_focused(window)),
        )
}

/// The header's face, one notch up from the row's so the title band reads as
/// being about *that* child.
const AVATAR_LARGE: f32 = 20.0;

fn footer(route: &Route) -> impl IntoElement {
    float::footer_band()
        .when(matches!(route, Route::List), |band| {
            band.child(float::key_hint_pair(
                IconName::ArrowUp,
                IconName::ArrowDown,
                "Navigate",
            ))
            .child(float::key_hint(IconName::ArrowRight, "Open"))
        })
        .when(matches!(route, Route::Thread(_)), |band| {
            band.child(float::key_hint(IconName::ArrowLeft, "Back"))
        })
        .child(div().flex_1().min_w_0())
}

fn body(state: &Subagents, route: &Route, app: &Entity<Luma>, window: &Window) -> AnyElement {
    match route {
        Route::List => list(state, app, window),
        Route::Thread(child) => reader(state, child),
    }
}

/// The child's own transcript, read-only. Nothing is drawn here — the panel
/// draws itself; this only gives it the card's remaining room.
fn reader(state: &Subagents, child: &SharedString) -> AnyElement {
    let Some((_, panel)) = state.reader.as_ref().filter(|(id, _)| id == child) else {
        // A paint-only morph copy of a route whose reader has already been
        // dropped. Empty rather than absent: the layer still has to lay out.
        return div().size_full().into_any_element();
    };
    div()
        .size_full()
        .overflow_hidden()
        .child(panel.clone())
        .into_any_element()
}

fn list(state: &Subagents, app: &Entity<Luma>, window: &Window) -> AnyElement {
    if state.rows.is_empty() {
        let message = "No subagents on this conversation";
        return float::viewport()
            .child(float::list().child(float::empty_row(message).agent_node(Role::Text, message)))
            .into_any_element();
    }
    // A plain scrolled column, not a virtualized list: a turn delegates to at
    // most four children at a time, and `uniform_list` would drop the
    // off-screen ones out of the dialog's tab ring.
    float::viewport()
        .child(
            float::list()
                .id("subagents-list")
                .overflow_y_scroll()
                .children(
                    state
                        .rows
                        .iter()
                        .map(|row| self_row(state, row, app, window)),
                ),
        )
        .into_any_element()
}

fn self_row(
    state: &Subagents,
    row: &SubagentSnapshot,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    let child = SharedString::from(row.child_thread_id.clone());
    let focus = state.row_focuses.get(&child);
    let focused = focus.is_some_and(|handle| handle.is_focused(window));
    let opened = app.clone();
    let picked = child.clone();
    let description = SharedString::from(row.description.clone());
    let mut pressable = float::menu_row(RowState::of(false, focused), format!("subagent-{child}"))
        .id(gpui::ElementId::Name(child.clone()))
        .tab_index(0)
        .w_full()
        .h(px(ROW_HEIGHT))
        .px(px(10.0))
        .gap(px(12.0))
        .on_click(move |_, window, cx| {
            let picked = picked.clone();
            opened.update(cx, |this, cx| this.open_subagent(picked, window, cx));
        });
    if let Some(focus) = focus {
        pressable = pressable.track_focus(focus);
    }
    pressable
        .child(luma_chat::subagents::avatar(&row.call_id, AVATAR_LARGE))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(
                    div()
                        .truncate()
                        .text_size(px(14.0))
                        .font_weight(FontWeight::MEDIUM)
                        .child(description.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(ladder::muted_foreground())
                        .child(phase_line(row)),
                ),
        )
        .agent_node(Role::Card, format!("{description} · {}", phase_line(row)))
        .agent_focused(focused)
        .into_any_element()
}

/// What a child is doing, in one line.
///
/// The live activity when there is one, the phase otherwise — the same
/// fallback the snapshot's own contract implies: `activity` is `None` before a
/// child's first tool call and between calls, and a row that went blank in
/// those gaps would read as a stalled subagent.
fn phase_line(row: &SubagentSnapshot) -> SharedString {
    match row.phase {
        SubagentPhase::Running => row
            .activity
            .clone()
            .map_or_else(|| "Working…".into(), SharedString::from),
        SubagentPhase::Merging => "Publishing its work…".into(),
        SubagentPhase::Completed => "Finished".into(),
        SubagentPhase::Failed => "Failed — nothing was applied".into(),
        // `SubagentPhase` is `#[non_exhaustive]`: a phase this build does not
        // know is still a child doing something, and a blank line would read
        // as a stalled one.
        _ => "Working…".into(),
    }
}
