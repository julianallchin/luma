//! A track's conversations, and a way to grep them.
//!
//! # What it lists, and why that is the subject
//!
//! The picker opens over whatever the chat is about — a track, or a pattern —
//! and lists that subject's conversations only. Threads are almost never
//! titled, so a row is named by its own words: what was first asked over what
//! was last answered. Every row is about the same thing, so nothing on it says
//! what that thing is; the empty state does.
//!
//! Typing turns the list into a grep. A query is matched against every line
//! said in those conversations, not against the rows' summaries — "where did
//! we talk about the drop" is a question about the transcripts, and a filter
//! over two-line summaries would answer it wrongly most of the time. Hits are
//! grouped under the conversation they came from; the grouping reads because
//! the rows are lines, and a line without its conversation is a quote without
//! a source.
//!
//! # Opening one changes only the conversation
//!
//! Picking a row does **not** move the workspace. Reading what was said about
//! something is not the same act as going back to it, and a picker that
//! rearranged the tabs would make browsing history expensive. The agent
//! re-orients itself from the transcript it is handed.
//!
//! The thread is opened **by id**. `resolve_thread` answers "the newest thread
//! for this subject", so routing a pick through it would land the reader in
//! whichever conversation happens to be newest — a click that silently opens
//! something else. [`luma_chat::AgentChat::open_thread`] pins the id it was
//! given.

use std::collections::HashMap;

use gpui::{
    div, prelude::*, px, Context, ElementId, Entity, FocusHandle, Focusable as _, FontWeight,
    KeyDownEvent, ScrollHandle, SharedString, Subscription,
};
use gpui_component::IconName;
use luma_lib::agent::{History, HistoryHit, ThreadEntry, ThreadScope};
use luma_ui::dialog::morph::{self, MorphSize};
use luma_ui::float::{self, Picker, RowState};
use luma_ui::ladder;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::text_input::{self, TextInput};

use crate::shell::{Body, Overlay};
use crate::welcome::relative_age;
use crate::Luma;

/// The card. One route, so the size is a constant rather than a morph: there is
/// no second state for the frame to travel to.
const CARD_SIZE: MorphSize = MorphSize::new(680.0, 460.0);
/// A conversation row: two lines — what was asked, and what was last answered.
const THREAD_ROW_HEIGHT: f32 = 46.0;
/// A grep hit: one line of one conversation.
const HIT_ROW_HEIGHT: f32 = 28.0;

/// What one row of the list is. The list is either all conversations or all
/// hits — never a mix — and the variant is what the query decides.
enum Row {
    /// Index into [`History::entries`].
    Thread(usize),
    /// A grep hit, and whether it is the first of its conversation — the one
    /// the conversation's heading is painted over.
    Hit { hit: HistoryHit, first: bool },
}

impl Row {
    /// Which conversation this row opens.
    fn entry(&self) -> usize {
        match self {
            Row::Thread(at) => *at,
            Row::Hit { hit, .. } => hit.entry,
        }
    }

    /// Stable within one query's rows: a hit's ordinal, a thread's index.
    fn key(&self, ordinal: usize) -> String {
        match self {
            Row::Thread(at) => format!("thread-{at}"),
            Row::Hit { .. } => format!("hit-{ordinal}"),
        }
    }
}

/// One picker instance.
///
/// `history` is `None` until the read lands, and that is a different state
/// from an empty list: an empty list is "you have not talked about this yet",
/// and the first frame of an async read is not that. Telling them apart is the
/// whole difference between an honest empty state and a lie.
pub(crate) struct ChatHistory {
    /// Which read this state belongs to. A slow list arriving after the reader
    /// has closed and reopened the dialog is a stale answer, and this is what
    /// lets it be dropped rather than painted over the new one.
    generation: u64,
    /// What the conversations are about, as the empty state names it. `None`
    /// when the chat is attached to nothing.
    subject: Option<String>,
    history: Option<History>,
    /// The query → rows → cursor loop, shared with every other searchable
    /// list. The rows are rebuilt from `history` on every query change, so the
    /// picker's own match rule is a pass-through: what "matches" means here is
    /// [`History::search`], and it runs before the picker sees a row.
    picker: Picker<Row>,
    error: Option<String>,
    search: Entity<TextInput>,
    search_focus: FocusHandle,
    close_focus: FocusHandle,
    /// One handle per row. A `tab_index` alone makes an element a stop that
    /// cannot *hold* focus, and gpui then settles focus on the nearest
    /// focusable ancestor — the dialog container — which reads as a broken
    /// ring. Every stop needs a handle of its own.
    row_focuses: HashMap<String, FocusHandle>,
    list_scroll: ScrollHandle,
    _search_subscription: Subscription,
}

impl ChatHistory {
    fn loading(generation: u64, subject: Option<String>, cx: &mut Context<Luma>) -> Self {
        let search = cx.new(|cx| TextInput::search("Search chats…", cx));
        let search_focus = search.read(cx).focus_handle(cx);
        let subscription = cx.subscribe(&search, |luma, field, event, cx| {
            if event == &text_input::Event::Edited {
                let query = field.read(cx).text().to_string();
                luma.chat_history_query_changed(query, cx);
            } else {
                cx.notify();
            }
        });
        Self {
            generation,
            subject,
            history: None,
            picker: Picker::new(|_, _| true),
            error: None,
            search,
            search_focus,
            close_focus: cx.focus_handle().tab_stop(true),
            row_focuses: HashMap::new(),
            list_scroll: ScrollHandle::new(),
            _search_subscription: subscription,
        }
    }

    fn finish(&mut self, history: History, cx: &mut Context<Luma>) {
        self.history = Some(history);
        self.error = None;
        self.rebuild_rows(cx);
    }

    fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.history = Some(History::default());
    }

    /// Rows for the current query — every conversation, or every hit — with
    /// one focus handle each, so every row is reachable by keyboard before
    /// anything is painted.
    fn rebuild_rows(&mut self, cx: &mut Context<Luma>) {
        let Some(history) = &self.history else {
            return;
        };
        let query = self.picker.query().trim();
        let rows: Vec<Row> = if query.is_empty() {
            (0..history.entries().len()).map(Row::Thread).collect()
        } else {
            let mut last = None;
            history
                .search(query)
                .into_iter()
                .map(|hit| {
                    let first = last != Some(hit.entry);
                    last = Some(hit.entry);
                    Row::Hit { hit, first }
                })
                .collect()
        };
        self.row_focuses = rows
            .iter()
            .enumerate()
            .map(|(ordinal, row)| (row.key(ordinal), cx.focus_handle().tab_stop(true)))
            .collect();
        self.picker.set_rows(rows);
    }

    fn entry(&self, row: &Row) -> &ThreadEntry {
        &self
            .history
            .as_ref()
            .expect("rows exist only once the history has loaded")
            .entries()[row.entry()]
    }
}

impl Luma {
    /// Open the history picker over the chat's subject.
    pub(crate) fn show_chat_history(&mut self, cx: &mut Context<Self>) {
        let scope = self
            .chat
            .as_ref()
            .and_then(|chat| chat.read(cx).scope().cloned());
        let subject = scope.as_ref().and_then(|scope| self.subject_name(scope));
        self.chat_history_generation = self.chat_history_generation.wrapping_add(1);
        let generation = self.chat_history_generation;
        let mut state = ChatHistory::loading(generation, subject, cx);
        // No subject, nothing to list — but the dialog still opens and says
        // so. Returning early here would be a button that does nothing when
        // pressed, which is indistinguishable from a broken one.
        let pending = scope.map(|scope| self.library.agent().history(scope));
        if pending.is_none() {
            state.finish(History::default(), cx);
        }
        self.overlay.open(Overlay::ChatHistory(Box::new(state)));
        cx.notify();
        let Some(pending) = pending else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let listed = pending.await;
            this.update(cx, |this, cx| {
                let Some(Overlay::ChatHistory(state)) = this.overlay.open_mut() else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                match listed {
                    Ok(history) => state.finish(history, cx),
                    Err(error) => state.fail(format!("Failed to load chats: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// What the chat's subject is called, from the tab that shows it. The
    /// subject's name is not the thread's data — it is the track's, or the
    /// pattern's — and the workspace already holds it.
    fn subject_name(&self, scope: &ThreadScope) -> Option<String> {
        self.workspace.iter().find_map(|tab| match &tab.body {
            Body::TrackEditor(editor)
                if editor
                    .subject()
                    .is_some_and(|(track, _, _)| track == scope.subject_id) =>
            {
                Some(editor.track_name().to_string())
            }
            Body::Graph(editor)
                if editor
                    .subject()
                    .is_some_and(|(pattern, _)| pattern == scope.subject_id) =>
            {
                Some(editor.pattern_name().to_string())
            }
            _ => None,
        })
    }

    pub(crate) fn chat_history_query_changed(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(Overlay::ChatHistory(state)) = self.overlay.open_mut() else {
            return;
        };
        if state.picker.query() == query {
            return;
        }
        state.picker.set_query(query);
        state.rebuild_rows(cx);
        state.list_scroll.scroll_to_item(0);
        cx.notify();
    }

    fn step_chat_history(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(Overlay::ChatHistory(state)) = self.overlay.open_mut() {
            if let Some(at) = state.picker.step(delta) {
                state.list_scroll.scroll_to_item(at);
                cx.notify();
            }
        }
    }

    fn open_active_chat(&mut self, cx: &mut Context<Self>) {
        let id = match self.overlay.as_open() {
            Some(Overlay::ChatHistory(state)) => state
                .picker
                .current()
                .map(|row| state.entry(row).thread.id.clone()),
            _ => None,
        };
        if let Some(id) = id {
            self.open_chat_thread(&id, cx);
        }
    }

    /// Show one conversation and close the picker. **Only the thread moves** —
    /// see the module docs.
    pub(crate) fn open_chat_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if let Some(chat) = self.chat.clone() {
            chat.update(cx, |chat, cx| chat.open_thread(thread_id, cx));
        }
        self.dismiss_overlay(cx);
    }

    pub(crate) fn chat_history_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if !matches!(self.overlay.as_open(), Some(Overlay::ChatHistory(_))) {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.dismiss_overlay(cx),
            "up" => self.step_chat_history(-1, cx),
            "down" => self.step_chat_history(1, cx),
            "enter" | "right" => self.open_active_chat(cx),
            _ => {}
        }
    }
}

/// Focus lands on the search field once, when the list is first painted.
pub(crate) fn tick(state: &mut ChatHistory, window: &mut gpui::Window, cx: &mut Context<Luma>) {
    if !state.search_focus.is_focused(window) && !state.close_focus.is_focused(window) {
        let on_a_row = state
            .row_focuses
            .values()
            .any(|handle| handle.is_focused(window));
        if !on_a_row {
            window.focus(&state.search_focus, cx);
        }
    }
}

/// The picker's card. One route, so [`morph::fixed_card`] rather than a morph:
/// the frame has nowhere to travel to, and a `MorphDialog` with a single route
/// would be machinery describing a transition that cannot happen.
pub(crate) fn render(
    state: &ChatHistory,
    app: &Entity<Luma>,
    window: &gpui::Window,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    morph::fixed_card(
        "Chat history dialog",
        CARD_SIZE,
        body(state, app, window, cx).into_any_element(),
    )
}

fn body(
    state: &ChatHistory,
    app: &Entity<Luma>,
    window: &gpui::Window,
    cx: &mut gpui::App,
) -> impl IntoElement {
    let keys = app.clone();
    div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_color(ladder::foreground())
        // No `track_focus`: the dialog host already owns this card's focus trap,
        // and a second focus container would add a stop with no control on it.
        // `on_key_down` only has to be an ancestor of whatever holds focus.
        .on_key_down(move |event, _, cx| {
            let event = event.clone();
            keys.update(cx, |this, cx| this.chat_history_key(&event, cx));
        })
        .child(header(state, app, window))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(list(state, app, window, cx)),
        )
        .child(footer())
}

fn header(state: &ChatHistory, app: &Entity<Luma>, window: &gpui::Window) -> impl IntoElement {
    let close = app.clone();
    let value = state.picker.query();
    let label = if value.is_empty() {
        "Search chats…"
    } else {
        value
    };
    float::header_band()
        .child(
            // The field is its own tab stop; this slot only sizes it. The
            // semantic label is the VALUE once there is one — a driver asking
            // what this field says wants what is in it, not what it would say
            // if empty.
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(14.0))
                .child(state.search.clone())
                .agent_node(Role::Input, label.to_string())
                .agent_focused(state.search_focus.is_focused(window)),
        )
        .child(
            float::key_cap_pressable(float::key_cap())
                .id("close-chat-history")
                .track_focus(&state.close_focus)
                .tab_index(0)
                .on_click(move |_, _, cx| close.update(cx, |this, cx| this.dismiss_overlay(cx)))
                .child("esc")
                .agent_node(Role::Button, "Close")
                .agent_focused(state.close_focus.is_focused(window)),
        )
}

fn footer() -> impl IntoElement {
    float::footer_band()
        .child(float::key_hint_pair(
            IconName::ArrowUp,
            IconName::ArrowDown,
            "Navigate",
        ))
        .child(float::key_hint(IconName::ArrowRight, "Open"))
        .child(div().flex_1().min_w_0())
}

fn list(
    state: &ChatHistory,
    app: &Entity<Luma>,
    window: &gpui::Window,
    cx: &mut gpui::App,
) -> gpui::AnyElement {
    if let Some(error) = &state.error {
        return float::viewport()
            .child(
                float::list()
                    .child(float::error_row(error.clone()).agent_node(Role::Text, error.clone())),
            )
            .into_any_element();
    }
    let Some(history) = &state.history else {
        return float::viewport()
            .child(
                float::list().child(
                    float::skeleton_rows(5, app.entity_id(), cx)
                        // Named, or a driver cannot tell a still-loading list
                        // from an empty one.
                        .agent_node(Role::Text, "Loading chats…"),
                ),
            )
            .into_any_element();
    };
    if state.picker.is_empty() {
        let message = match &state.subject {
            None => "No track or pattern open".to_string(),
            Some(subject) if history.is_empty() => format!("No chats about {subject} yet"),
            Some(_) => "No matches".to_string(),
        };
        return float::viewport()
            .child(
                float::list()
                    .child(float::empty_row(message.clone()).agent_node(Role::Text, message)),
            )
            .into_any_element();
    }
    // A plain tracked column, not a virtualized list: a subject holds tens of
    // conversations, and a `uniform_list` renders only what is on screen —
    // which would silently drop the off-screen rows out of the dialog's tab
    // ring. Every conversation has to stay reachable by keyboard.
    float::viewport()
        .child(
            float::list()
                .id("chat-history-list")
                .overflow_y_scroll()
                .track_scroll(&state.list_scroll)
                .children(state.picker.shown().enumerate().map(|(index, row)| {
                    let focus = state
                        .row_focuses
                        .get(&row.key(index))
                        .expect("a listed row has no focus handle");
                    let entry = state.entry(row);
                    let cursor = index == state.picker.cursor();
                    match row {
                        Row::Thread(_) => {
                            thread_row(entry, cursor, focus, app, window).into_any_element()
                        }
                        Row::Hit { hit, first } => {
                            hit_row(entry, hit, index, *first, cursor, focus, app, window)
                                .into_any_element()
                        }
                    }
                })),
        )
        .into_any_element()
}

/// The pressable shell every row shares: focus, tab stop, click-to-open.
fn pressable(
    entry: &ThreadEntry,
    key: String,
    cursor: bool,
    focus: &FocusHandle,
    app: &Entity<Luma>,
) -> gpui::Stateful<gpui::Div> {
    let id = entry.thread.id.clone();
    let opened = app.clone();
    float::menu_row(RowState::of(false, cursor), format!("chat-{key}"))
        .id(ElementId::Name(SharedString::from(key)))
        .track_focus(focus)
        .tab_index(0)
        .w_full()
        .px(px(10.0))
        .gap(px(12.0))
        .on_click(move |_, _, cx| {
            opened.update(cx, |this, cx| this.open_chat_thread(&id, cx));
        })
}

fn age(entry: &ThreadEntry) -> gpui::Div {
    div()
        .flex_none()
        .text_size(px(11.0))
        .text_color(ladder::muted_foreground())
        .child(relative_age(&entry.thread.updated_at))
}

fn thread_row(
    entry: &ThreadEntry,
    cursor: bool,
    focus: &FocusHandle,
    app: &Entity<Luma>,
    window: &gpui::Window,
) -> impl IntoElement {
    let headline = entry.headline();
    pressable(entry, entry.thread.id.clone(), cursor, focus, app)
        .h(px(THREAD_ROW_HEIGHT))
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
                        .child(headline.clone()),
                )
                .when_some(entry.latest.clone(), |column, latest| {
                    column.child(
                        div()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(ladder::muted_foreground())
                            .child(latest),
                    )
                }),
        )
        .child(age(entry))
        .agent_node(Role::Card, headline)
        .agent_focused(focus.is_focused(window))
}

/// One grep hit. The first hit of a conversation carries the conversation's
/// heading — its age — above it, so the group reads without a row of its own
/// that the cursor would have to skip.
#[allow(clippy::too_many_arguments)]
fn hit_row(
    entry: &ThreadEntry,
    hit: &HistoryHit,
    ordinal: usize,
    first: bool,
    cursor: bool,
    focus: &FocusHandle,
    app: &Entity<Luma>,
    window: &gpui::Window,
) -> impl IntoElement {
    let before = hit.excerpt[..hit.span.start].to_string();
    let matched = hit.excerpt[hit.span.clone()].to_string();
    let after = hit.excerpt[hit.span.end..].to_string();
    div()
        .flex()
        .flex_col()
        .when(first, |column| {
            column.child(
                float::section_heading(format!(
                    "{} · {}",
                    relative_age(&entry.thread.updated_at),
                    entry.headline()
                ))
                .pt(px(6.0))
                .truncate(),
            )
        })
        .child(
            pressable(entry, format!("hit-{ordinal}"), cursor, focus, app)
                .h(px(HIT_ROW_HEIGHT))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(13.0))
                        .text_color(ladder::muted_foreground())
                        .child(before)
                        .child(
                            div()
                                .flex_none()
                                .font_weight(FontWeight::BOLD)
                                .text_color(ladder::foreground())
                                .child(matched),
                        )
                        .child(div().truncate().child(after)),
                )
                .agent_node(Role::Card, hit.excerpt.clone())
                .agent_focused(focus.is_focused(window)),
        )
}
