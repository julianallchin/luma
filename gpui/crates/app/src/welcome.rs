//! The venue chooser and first-run create flow.
//!
//! This is route content inside the shell's one shared dialog host. The host
//! owns the full-window scrim, shell/sidebar backdrop samples, foreground
//! frost and focus trap; this module owns only venue navigation state.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::scroll::ScrollableElement as _;
use luma_lib::models::venues::Venue;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::{ladder, Enabled};

use crate::{shell::Overlay, Luma};

pub(crate) const LAST_VENUE: &str = "last-venue";
const CARD_HEIGHT: f32 = 54.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Browse,
    Create,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusTarget {
    Search,
    CreateName,
}

/// One picker instance. `loaded` is deliberately separate from `venues`: an
/// empty catalogue is onboarding, not the first frame of every async read.
pub(crate) struct VenuePicker {
    generation: u64,
    venues: Rc<[Venue]>,
    shown: Rc<[Venue]>,
    loaded: bool,
    error: Option<String>,
    query: String,
    route: Route,
    create_name: String,
    create_error: Option<String>,
    creating: bool,
    search_focus: FocusHandle,
    create_focus: FocusHandle,
    patterns_focus: FocusHandle,
    browse_create_focus: FocusHandle,
    venue_focuses: HashMap<String, FocusHandle>,
    focus_pending: Option<FocusTarget>,
}

impl VenuePicker {
    fn loading(generation: u64, cx: &mut Context<Luma>) -> Self {
        Self {
            generation,
            venues: Rc::from(Vec::new()),
            shown: Rc::from(Vec::new()),
            loaded: false,
            error: None,
            query: String::new(),
            route: Route::Browse,
            create_name: String::new(),
            create_error: None,
            creating: false,
            search_focus: cx.focus_handle().tab_stop(true),
            create_focus: cx.focus_handle().tab_stop(true),
            patterns_focus: cx.focus_handle().tab_stop(true),
            browse_create_focus: cx.focus_handle().tab_stop(true),
            venue_focuses: HashMap::new(),
            focus_pending: None,
        }
    }

    fn finish(&mut self, venues: Vec<Venue>, cx: &mut Context<Luma>) {
        self.venue_focuses = venues
            .iter()
            .map(|venue| (venue.id.clone(), cx.focus_handle().tab_stop(true)))
            .collect();
        self.venues = venues.into();
        self.loaded = true;
        self.error = None;
        if self.venues.is_empty() {
            self.route = Route::Create;
            self.focus_pending = Some(FocusTarget::CreateName);
        } else {
            self.route = Route::Browse;
            self.focus_pending = Some(FocusTarget::Search);
        }
        self.refilter();
    }

    fn fail(&mut self, error: impl Into<String>) {
        self.loaded = true;
        self.error = Some(error.into());
        self.shown = Rc::from(Vec::new());
    }

    fn refilter(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.shown = self
            .venues
            .iter()
            .filter(|venue| {
                query.is_empty()
                    || venue.name.to_lowercase().contains(&query)
                    || venue
                        .description
                        .as_deref()
                        .is_some_and(|description| description.to_lowercase().contains(&query))
            })
            .cloned()
            .collect::<Vec<_>>()
            .into();
    }
}

impl Luma {
    fn next_venue_picker_generation(&mut self) -> u64 {
        self.venue_picker_generation = self.venue_picker_generation.wrapping_add(1);
        self.venue_picker_generation
    }

    /// First paint is an explicit loading dialog. Once both durable facts
    /// arrive, a valid remembered venue opens; every other state stays in the
    /// picker with a truthful loading/empty/error route.
    pub(crate) fn restore_venue(&mut self, cx: &mut Context<Self>) {
        let generation = self.next_venue_picker_generation();
        self.overlay = Some(Overlay::Venues(VenuePicker::loading(generation, cx)));
        let venues = self.library.venues();
        let remembered = self.library.get_session_item(LAST_VENUE);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let venues = venues.await;
            let remembered = remembered.await;
            this.update(cx, |this, cx| {
                let Some(Overlay::Venues(state)) = &mut this.overlay else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                let rows = match venues {
                    Ok(rows) => rows,
                    Err(error) => {
                        state.fail(format!("Failed to load venues: {error}"));
                        cx.notify();
                        return;
                    }
                };
                let remembered = match remembered {
                    Ok(remembered) => remembered,
                    Err(error) => {
                        state.fail(format!("Failed to restore last venue: {error}"));
                        cx.notify();
                        return;
                    }
                };
                let restore = remembered
                    .as_deref()
                    .and_then(|id| rows.iter().find(|venue| venue.id == id))
                    .cloned();
                if let Some(venue) = restore {
                    this.open_venue(venue, cx);
                    return;
                }
                if remembered.is_some() {
                    let clear = this.library.remove_session_item(LAST_VENUE);
                    cx.spawn(async move |_, _| {
                        let _ = clear.await;
                    })
                    .detach();
                }
                if let Some(Overlay::Venues(state)) = &mut this.overlay {
                    if state.generation == generation {
                        state.finish(rows, cx);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Reopen the same shared picker from the sidebar, with a freshly
    /// correlated catalogue read and no startup restore decision.
    pub(crate) fn show_venues(&mut self, cx: &mut Context<Self>) {
        let generation = self.next_venue_picker_generation();
        self.overlay = Some(Overlay::Venues(VenuePicker::loading(generation, cx)));
        let pending = self.library.venues();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let Some(Overlay::Venues(state)) = &mut this.overlay else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                match result {
                    Ok(rows) => state.finish(rows, cx),
                    Err(error) => state.fail(format!("Failed to load venues: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn venue_search_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        self.with_venue_picker(cx, |state| {
            match keystroke.key.as_str() {
                "backspace" => {
                    state.query.pop();
                }
                "escape" => state.query.clear(),
                _ => match keystroke.key_char.as_deref() {
                    Some(text) if !text.contains(['\n', '\t']) => state.query.push_str(text),
                    _ => return,
                },
            }
            state.refilter();
        });
    }

    fn venue_create_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        self.with_venue_picker(cx, |state| {
            match keystroke.key.as_str() {
                "backspace" => {
                    state.create_name.pop();
                }
                _ => match keystroke.key_char.as_deref() {
                    Some(text) if !text.contains(['\n', '\t']) => state.create_name.push_str(text),
                    _ => return,
                },
            }
            state.create_error = None;
        });
    }

    fn show_create_venue(&mut self, cx: &mut Context<Self>) {
        self.with_venue_picker(cx, |state| {
            state.route = Route::Create;
            state.create_error = None;
            state.focus_pending = Some(FocusTarget::CreateName);
        });
    }

    fn cancel_create_venue(&mut self, cx: &mut Context<Self>) {
        self.with_venue_picker(cx, |state| {
            if !state.venues.is_empty() && !state.creating {
                state.route = Route::Browse;
                state.focus_pending = Some(FocusTarget::Search);
            }
        });
    }

    fn create_venue(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::Venues(state)) = &mut self.overlay else {
            return;
        };
        let name = state.create_name.trim().to_string();
        if name.is_empty() || state.creating {
            return;
        }
        state.creating = true;
        state.create_error = None;
        let generation = state.generation;
        let pending = self.library.create_venue(&name, None);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let current = matches!(
                    &this.overlay,
                    Some(Overlay::Venues(state)) if state.generation == generation
                );
                if !current {
                    return;
                }
                match result {
                    Ok(venue) => this.open_venue(venue, cx),
                    Err(error) => {
                        if let Some(Overlay::Venues(state)) = &mut this.overlay {
                            state.creating = false;
                            state.create_error = Some(format!("Failed to create venue: {error}"));
                            cx.notify();
                        }
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_picker_venue(&mut self, id: &str, cx: &mut Context<Self>) {
        let venue = match &self.overlay {
            Some(Overlay::Venues(state)) => {
                state.venues.iter().find(|venue| venue.id == id).cloned()
            }
            _ => None,
        };
        if let Some(venue) = venue {
            self.open_venue(venue, cx);
        }
    }

    fn with_venue_picker(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut VenuePicker)) {
        if let Some(Overlay::Venues(state)) = &mut self.overlay {
            edit(state);
            cx.notify();
        }
    }
}

/// Focus is committed after the async route decision, never while the loading
/// plate is pretending to be an input. Called before the overlay is rendered.
pub(crate) fn tick(state: &mut VenuePicker, window: &mut Window, cx: &mut Context<Luma>) {
    let Some(target) = state.focus_pending.take() else {
        return;
    };
    match target {
        FocusTarget::Search => window.focus(&state.search_focus, cx),
        FocusTarget::CreateName => window.focus(&state.create_focus, cx),
    }
}

pub(crate) fn welcome(state: &VenuePicker, app: &Entity<Luma>, window: &Window) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .text_color(ladder::foreground())
        .child(toolbar(state, app, window))
        .child(match state.route {
            Route::Browse => browser(state, app, window).into_any_element(),
            Route::Create => create(state, app, window).into_any_element(),
        })
}

fn toolbar(state: &VenuePicker, app: &Entity<Luma>, window: &Window) -> Div {
    let patterns = app.clone();
    let create = app.clone();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(12.))
        .h(px(48.0))
        .px(px(12.))
        .bg(luma_ui::glass::band())
        .border_b_1()
        .border_color(luma_ui::glass::hairline(0.08))
        .child(
            div()
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(if state.route == Route::Create {
                    "Create venue"
                } else {
                    "Choose a venue"
                })
                .agent_node(
                    Role::Text,
                    if state.route == Route::Create {
                        "Create venue"
                    } else {
                        "Choose a venue"
                    },
                ),
        )
        .when(
            state.route == Route::Browse && state.loaded && state.error.is_none(),
            |bar| bar.child(search(state, app, window)),
        )
        .child(div().flex_1())
        .when(
            state.route == Route::Browse && state.loaded && state.error.is_none(),
            |bar| {
                bar.child(
                    picker_button("Patterns", false, Enabled::Yes)
                        .id("patterns")
                        .track_focus(&state.patterns_focus)
                        .on_click(move |_, _, cx| {
                            patterns.update(cx, |this, cx| this.show_patterns(cx))
                        })
                        .agent_node(Role::Button, "Patterns")
                        .agent_focused(state.patterns_focus.is_focused(window)),
                )
                .child(
                    picker_button("Create venue", true, Enabled::Yes)
                        .id("create-venue")
                        .track_focus(&state.browse_create_focus)
                        .on_click(move |_, _, cx| {
                            create.update(cx, |this, cx| this.show_create_venue(cx))
                        })
                        .agent_node(Role::Button, "Create venue")
                        .agent_focused(state.browse_create_focus.is_focused(window)),
                )
            },
        )
}

fn search(state: &VenuePicker, app: &Entity<Luma>, window: &Window) -> impl IntoElement {
    let empty = state.query.is_empty();
    let label = if empty {
        "Search venues…".to_string()
    } else {
        state.query.clone()
    };
    let typed = app.clone();
    div()
        .id("venue-search")
        .tab_index(0)
        .w(px(240.))
        .h(px(32.))
        .px(px(11.))
        .flex()
        .items_center()
        .rounded(px(7.))
        .bg(luma_ui::glass::wash(0.04))
        .border_1()
        .border_color(luma_ui::glass::hairline(0.08))
        .text_size(px(12.))
        .text_color(if empty {
            ladder::muted_foreground()
        } else {
            ladder::foreground()
        })
        .track_focus(&state.search_focus)
        .key_context(crate::keymap::context::TEXT_INPUT)
        .on_key_down(move |event, _, cx| {
            let key = event.keystroke.clone();
            typed.update(cx, |this, cx| this.venue_search_key(&key, cx));
        })
        .child(label.clone())
        .agent_node(Role::Input, label)
        .agent_focused(state.search_focus.is_focused(window))
}

fn browser(state: &VenuePicker, app: &Entity<Luma>, window: &Window) -> Div {
    let content = match &state.error {
        Some(error) => {
            let retry = app.clone();
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .child(
                    div()
                        .w_full()
                        .max_w(px(560.))
                        .h(px(36.))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .child(
                            // GPUI wraps at whitespace but has no
                            // break-anywhere mode. A backend can hand us
                            // thousands of lines or one enormous token, so
                            // the visual surface is a bounded ellipsis while
                            // the semantic node retains the complete error.
                            div()
                                .w_full()
                                .truncate()
                                .text_center()
                                .text_color(ladder::danger())
                                .child(error.clone())
                                .agent_node(Role::Text, error.clone()),
                        )
                        .agent_node(Role::Card, "Venue error viewport"),
                )
                .child(
                    picker_button("Retry", true, Enabled::Yes)
                        .id("retry-venues")
                        .on_click(move |_, _, cx| retry.update(cx, |this, cx| this.show_venues(cx)))
                        .agent_node(Role::Button, "Retry"),
                )
                .into_any_element()
        }
        None if !state.loaded => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .child("Loading venues…")
                    .agent_node(Role::Text, "Loading venues…"),
            )
            .into_any_element(),
        None if state.shown.is_empty() => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .child("No matching venues")
                    .agent_node(Role::Text, "No matching venues"),
            )
            .into_any_element(),
        None => venue_grid(state, app, window).into_any_element(),
    };
    div().flex_1().min_h_0().child(content)
}

fn venue_grid(state: &VenuePicker, app: &Entity<Luma>, window: &Window) -> impl IntoElement {
    div().size_full().overflow_y_scrollbar().p(px(8.)).child(
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(state.shown.iter().map(|venue| {
                venue_card(
                    venue,
                    state
                        .venue_focuses
                        .get(&venue.id)
                        .expect("loaded venue has no focus handle"),
                    app,
                    window,
                )
            })),
    )
}

fn venue_card(
    venue: &Venue,
    focus: &FocusHandle,
    app: &Entity<Luma>,
    window: &Window,
) -> impl IntoElement {
    let id = venue.id.clone();
    let opened = app.clone();
    div()
        .id(ElementId::Name(SharedString::from(id.clone())))
        .track_focus(focus)
        .tab_index(0)
        .w_full()
        .h(px(CARD_HEIGHT))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(12.0))
        .rounded(px(8.))
        .hover(|card| card.bg(luma_ui::glass::wash(0.08)))
        .on_click(move |_, _, cx| opened.update(cx, |this, cx| this.open_picker_venue(&id, cx)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(venue.name.clone()),
                )
                .when_some(venue.description.clone(), |column, description| {
                    column.child(
                        div()
                            .text_size(px(12.))
                            .text_color(ladder::muted_foreground())
                            .child(description),
                    )
                }),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(11.))
                .text_color(ladder::muted_foreground())
                .child(local_date(&venue.updated_at)),
        )
        .agent_node(Role::Card, venue.name.clone())
        .agent_focused(focus.is_focused(window))
}

fn create(state: &VenuePicker, app: &Entity<Luma>, window: &Window) -> Div {
    let typed = app.clone();
    let submit = app.clone();
    let cancel = app.clone();
    let empty = state.create_name.is_empty();
    let label = if empty {
        "Venue name".to_string()
    } else {
        state.create_name.clone()
    };
    div().flex_1().flex().items_center().justify_center().child(
        div()
            .w(px(420.))
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .text_size(px(24.))
                    .font_weight(FontWeight::LIGHT)
                    .child(if state.venues.is_empty() {
                        "Create your first venue"
                    } else {
                        "A new room to light"
                    })
                    .agent_node(
                        Role::Text,
                        if state.venues.is_empty() {
                            "Create your first venue"
                        } else {
                            "A new room to light"
                        },
                    ),
            )
            .child(
                div()
                    .id("venue-name")
                    .tab_index(0)
                    .h(px(38.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .bg(luma_ui::glass::wash(0.04))
                    .border_1()
                    .border_color(luma_ui::glass::hairline(0.08))
                    .text_color(if empty {
                        ladder::muted_foreground()
                    } else {
                        ladder::foreground()
                    })
                    .track_focus(&state.create_focus)
                    .key_context(crate::keymap::context::TEXT_INPUT)
                    .on_key_down(move |event, _, cx| {
                        let key = event.keystroke.clone();
                        typed.update(cx, |this, cx| this.venue_create_key(&key, cx));
                    })
                    .child(label.clone())
                    .agent_node(Role::Input, label)
                    .agent_focused(state.create_focus.is_focused(window)),
            )
            .when_some(state.create_error.clone(), |column, error| {
                column.child(
                    div()
                        .text_color(ladder::danger())
                        .child(error.clone())
                        .agent_node(Role::Text, error),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .when(!state.venues.is_empty(), |actions| {
                        actions.child(
                            picker_button(
                                "Cancel",
                                false,
                                if state.creating {
                                    Enabled::No
                                } else {
                                    Enabled::Yes
                                },
                            )
                            .id("cancel-create-venue")
                            .when(!state.creating, |button| {
                                button.on_click(move |_, _, cx| {
                                    cancel.update(cx, |this, cx| this.cancel_create_venue(cx))
                                })
                            })
                            .agent_node(Role::Button, "Cancel"),
                        )
                    })
                    .child(
                        picker_button(
                            if state.creating {
                                "Creating…"
                            } else {
                                "Create venue"
                            },
                            true,
                            if !empty && !state.creating {
                                Enabled::Yes
                            } else {
                                Enabled::No
                            },
                        )
                        .id("submit-create-venue")
                        .when(!empty && !state.creating, |button| {
                            button.on_click(move |_, _, cx| {
                                submit.update(cx, |this, cx| this.create_venue(cx))
                            })
                        })
                        .agent_node(
                            Role::Button,
                            if state.creating {
                                "Creating…"
                            } else {
                                "Create venue"
                            },
                        ),
                    ),
            ),
    )
}

fn picker_button(label: &str, primary: bool, enabled: Enabled) -> Div {
    div()
        .px(px(11.0))
        .py(px(6.0))
        .rounded(px(8.0))
        .text_size(px(13.0))
        .when(primary, |button| {
            button
                .bg(ladder::foreground())
                .text_color(ladder::background())
        })
        .when(!primary, |button| {
            button.text_color(ladder::muted_foreground())
        })
        .when(enabled == Enabled::Yes, |button| {
            button.tab_index(0).cursor_pointer().hover(move |state| {
                if primary {
                    state.opacity(0.9)
                } else {
                    state
                        .bg(luma_ui::glass::wash(0.08))
                        .text_color(ladder::foreground())
                }
            })
        })
        .when(enabled == Enabled::No, |button| button.opacity(0.38))
        .child(label.to_string())
}

fn local_date(timestamp: &str) -> String {
    let date = timestamp.get(..10).unwrap_or(timestamp);
    let mut parts = date.split('-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(year), Some(month), Some(day)) if year.len() == 4 => format!(
            "{}/{}/{year}",
            month.trim_start_matches('0'),
            day.trim_start_matches('0')
        ),
        _ => date.to_string(),
    }
}
