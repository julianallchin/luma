//! The venue chooser and first-run create flow.
//!
//! This is route content inside the shell's one shared dialog host. The host
//! owns the full-window scrim, shell/sidebar backdrop samples, foreground
//! frost and focus trap; this module owns only venue navigation state.
//!
//! # The same palette as the track picker
//!
//! Header band carrying a field and the committing chip, a list, a footer
//! legend — see `crate::add_tracks`, which this deliberately mirrors. Choosing
//! a venue and choosing a track are the same gesture, so they are the same
//! object; the only difference is what the list holds and what the field
//! filters.
//!
//! Both routes put their field in the header: on Browse it filters venues, on
//! Create it *is* the new venue's name. A route whose header emptied out on
//! the way to creating something would make the two feel like separate
//! screens rather than one picker with a second mode.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::IconName;
use luma_lib::models::venues::Venue;
use luma_ui::dialog::morph::{self, ContentMode, MorphDialog, MorphSize, RouteDescriptor};
use luma_ui::float::{self, RowState};
use luma_ui::ladder;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::text_input::{self, TextInput};

use crate::{shell::Overlay, Luma};

pub(crate) const LAST_VENUE: &str = "last-venue";
/// The one size both routes take. Like the track palette, browse and create
/// are the same object a step apart, so the card never resizes and the morph
/// spends its whole span on the content instead of the frame.
const PICKER_SIZE: MorphSize = MorphSize::new(680.0, 460.0);
/// Venue rows are taller than a track row: a venue carries a name, a
/// description and a date, and the row is the primary object on this screen
/// rather than one of hundreds.
const CARD_HEIGHT: f32 = 54.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Browse,
    Create,
}

impl Route {
    fn descriptor(self) -> RouteDescriptor<Self> {
        RouteDescriptor::exact(self, PICKER_SIZE.width, PICKER_SIZE.height)
    }
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
    morph: MorphDialog<Route>,
    create_name: String,
    create_error: Option<String>,
    creating: bool,
    /// The header band's two fields — see the module docs on why both routes
    /// put one there.
    search: Entity<TextInput>,
    name: Entity<TextInput>,
    search_focus: FocusHandle,
    create_focus: FocusHandle,
    /// One handle per header control. A `tab_index` alone makes an element a
    /// stop that cannot *hold* focus — gpui then settles focus on the nearest
    /// focusable ancestor, which is the dialog container, and the ring reads as
    /// broken. Every stop needs a handle of its own.
    /// The create route's Back cap. Browse has nothing in that slot — patterns
    /// are reached from the tab strip's + menu, not from the venue switcher:
    /// picking a room and picking a light pattern are different questions, and
    /// only one of them is what this dialog is for.
    leading_focus: FocusHandle,
    create_action_focus: FocusHandle,
    close_focus: FocusHandle,
    venue_focuses: HashMap<String, FocusHandle>,
    focus_pending: Option<FocusTarget>,
    /// Which venue the arrow keys are on. Not a selection: opening a venue is
    /// the only thing this list does, so there is no state to keep.
    active: usize,
    list_scroll: ScrollHandle,
    _field_subscriptions: [Subscription; 2],
}

impl VenuePicker {
    fn loading(generation: u64, cx: &mut Context<Luma>) -> Self {
        let search = cx.new(|cx| TextInput::search("Search venues…", cx));
        let name = cx.new(|cx| TextInput::search("Venue name", cx));
        let search_focus = search.read(cx).focus_handle(cx);
        let create_focus = name.read(cx).focus_handle(cx);
        let subscriptions = [
            cx.subscribe(&search, |luma, field, event, cx| {
                if event == &text_input::Event::Edited {
                    let query = field.read(cx).text().to_string();
                    luma.venue_query_changed(query, cx);
                } else {
                    cx.notify();
                }
            }),
            cx.subscribe(&name, |luma, field, event, cx| {
                if event == &text_input::Event::Edited {
                    let name = field.read(cx).text().to_string();
                    luma.venue_name_changed(name, cx);
                } else {
                    cx.notify();
                }
            }),
        ];
        Self {
            generation,
            venues: Rc::from(Vec::new()),
            shown: Rc::from(Vec::new()),
            loaded: false,
            error: None,
            query: String::new(),
            morph: MorphDialog::new(Route::Browse.descriptor(), PICKER_SIZE),
            create_name: String::new(),
            create_error: None,
            creating: false,
            search,
            name,
            search_focus,
            create_focus,
            leading_focus: cx.focus_handle().tab_stop(true),
            create_action_focus: cx.focus_handle().tab_stop(true),
            close_focus: cx.focus_handle().tab_stop(true),
            venue_focuses: HashMap::new(),
            focus_pending: None,
            active: 0,
            list_scroll: ScrollHandle::new(),
            _field_subscriptions: subscriptions,
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
            self.settle_route(Route::Create);
            self.focus_pending = Some(FocusTarget::CreateName);
        } else {
            self.settle_route(Route::Browse);
            self.focus_pending = Some(FocusTarget::Search);
        }
        self.refilter();
    }

    fn fail(&mut self, error: impl Into<String>) {
        self.loaded = true;
        self.error = Some(error.into());
        self.shown = Rc::from(Vec::new());
    }

    /// The route the picker is on, or arriving at. Reads through the morph so
    /// a request mid-flight already reports its destination.
    fn route(&self) -> Route {
        *self.morph.target_key()
    }

    /// Adopt `route` with no animation — for the first route, decided by what
    /// the catalogue turned out to hold. Morphing away from a route the user
    /// never saw is motion with nothing behind it.
    fn settle_route(&mut self, route: Route) {
        self.morph.request(route.descriptor(), Instant::now(), true);
    }

    /// Travel to `route`, animating unless motion is off. The flag is read by
    /// the caller: `with_venue_picker` already holds the context mutably.
    fn go(&mut self, route: Route, reduced: bool) {
        self.morph
            .request(route.descriptor(), Instant::now(), reduced);
    }

    fn refilter(&mut self) {
        self.active = 0;
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
        self.overlay
            .open(Overlay::Venues(Box::new(VenuePicker::loading(
                generation, cx,
            ))));
        let venues = self.library.venues();
        let remembered = self.library.get_session_item(LAST_VENUE);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let venues = venues.await;
            let remembered = remembered.await;
            this.update(cx, |this, cx| {
                let Some(Overlay::Venues(state)) = this.overlay.open_mut() else {
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
                if let Some(Overlay::Venues(state)) = this.overlay.open_mut() {
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
        self.overlay
            .open(Overlay::Venues(Box::new(VenuePicker::loading(
                generation, cx,
            ))));
        let pending = self.library.venues();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let Some(Overlay::Venues(state)) = this.overlay.open_mut() else {
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

    /// Mirror an edit of the venue filter into the model.
    fn venue_query_changed(&mut self, query: String, cx: &mut Context<Self>) {
        self.with_venue_picker(cx, |state| {
            state.query = query;
            state.refilter();
        });
    }

    /// Mirror an edit of the new venue's name, and clear a failure the user is
    /// now actively addressing.
    fn venue_name_changed(&mut self, name: String, cx: &mut Context<Self>) {
        self.with_venue_picker(cx, |state| {
            state.create_name = name;
            state.create_error = None;
        });
    }

    /// The picker's keyboard, in the same navigator shape as the track
    /// palette: arrows walk the list, Enter acts, ⌘⏎ commits, ⌫ on an empty
    /// field steps back.
    fn venue_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(Overlay::Venues(state)) = self.overlay.as_open() else {
            return;
        };
        let route = state.route();
        let field_empty = match route {
            Route::Browse => state.query.is_empty(),
            Route::Create => state.create_name.is_empty(),
        };
        let key = event.keystroke.key.as_str();
        let modified = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;

        match (route, key) {
            // Escape is refused while onboarding has no venue to fall back to
            // — `dismiss_overlay` owns that rule, so it is not repeated here.
            (_, "escape") => self.dismiss_overlay(cx),
            (Route::Create, "left") => self.cancel_create_venue(cx),
            (Route::Create, "backspace") if field_empty => self.cancel_create_venue(cx),
            // On the create route Enter alone commits: there is no list under
            // the cursor for it to mean anything else.
            (Route::Create, "enter") => self.create_venue(cx),
            (Route::Browse, "up") => self.step_venue(-1, cx),
            (Route::Browse, "down") => self.step_venue(1, cx),
            (Route::Browse, "enter") if modified => self.show_create_venue(cx),
            (Route::Browse, "enter" | "right") => self.open_active_venue(cx),
            _ => {}
        }
    }

    fn step_venue(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.with_venue_picker(cx, |state| {
            let count = state.shown.len();
            if count == 0 {
                return;
            }
            state.active = (state.active as isize + delta).rem_euclid(count as isize) as usize;
            state.list_scroll.scroll_to_item(state.active);
        });
    }

    fn open_active_venue(&mut self, cx: &mut Context<Self>) {
        let venue = match self.overlay.as_open() {
            Some(Overlay::Venues(state)) => state.shown.get(state.active).cloned(),
            _ => None,
        };
        if let Some(venue) = venue {
            self.open_venue(venue, cx);
        }
    }

    fn show_create_venue(&mut self, cx: &mut Context<Self>) {
        let reduced = luma_ui::motion::reduced_motion(cx);
        self.with_venue_picker(cx, move |state| {
            state.go(Route::Create, reduced);
            state.create_error = None;
            state.focus_pending = Some(FocusTarget::CreateName);
        });
    }

    fn cancel_create_venue(&mut self, cx: &mut Context<Self>) {
        let reduced = luma_ui::motion::reduced_motion(cx);
        self.with_venue_picker(cx, move |state| {
            if !state.venues.is_empty() && !state.creating {
                state.go(Route::Browse, reduced);
                state.focus_pending = Some(FocusTarget::Search);
            }
        });
    }

    fn create_venue(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::Venues(state)) = self.overlay.open_mut() else {
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
                    this.overlay.as_open(),
                    Some(Overlay::Venues(state)) if state.generation == generation
                );
                if !current {
                    return;
                }
                match result {
                    Ok(venue) => this.open_venue(venue, cx),
                    Err(error) => {
                        if let Some(Overlay::Venues(state)) = this.overlay.open_mut() {
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
        let venue = match self.overlay.as_open() {
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
        if let Some(Overlay::Venues(state)) = self.overlay.open_mut() {
            edit(state);
            cx.notify();
        }
    }
}

/// Focus is committed after the async route decision, never while the loading
/// plate is pretending to be an input. Called before the overlay is rendered.
pub(crate) fn tick(
    state: &mut VenuePicker,
    dialog_focus: &FocusHandle,
    window: &mut Window,
    cx: &mut Context<Luma>,
) {
    let now = Instant::now();
    let reduced = luma_ui::motion::reduced_motion(cx);
    if state.morph.tick(now, reduced) {
        window.request_animation_frame();
    }
    // In-flight route copies carry no focus handles, so focus parks on the
    // host scope for the flight rather than on a control that has unmounted.
    if state.morph.sample(now).animating {
        window.focus(dialog_focus, cx);
        return;
    }
    if let Some(route) = state.morph.take_focus_after_commit() {
        state.focus_pending = Some(match route {
            Route::Browse => FocusTarget::Search,
            Route::Create => FocusTarget::CreateName,
        });
    }
    let Some(target) = state.focus_pending.take() else {
        return;
    };
    match target {
        FocusTarget::Search => window.focus(&state.search_focus, cx),
        FocusTarget::CreateName => window.focus(&state.create_focus, cx),
    }
}

/// The venue picker's card. Like the track palette, this owns its own
/// `morph::card` — the shell hands it no box to live in.
pub(crate) fn render(
    state: &VenuePicker,
    app: &Entity<Luma>,
    window: &Window,
    cx: &mut gpui::App,
) -> AnyElement {
    let sample = state.morph.sample(Instant::now());
    let app = app.clone();
    morph::card(&sample, "Venue dialog", move |route, mode| {
        route_body(state, *route, mode, &app, window, cx)
    })
}

fn route_body(
    state: &VenuePicker,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
    cx: &mut gpui::App,
) -> AnyElement {
    let interactive = mode == ContentMode::Interactive;
    let keys = app.clone();
    let mut frame = div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .text_color(ladder::foreground());
    if interactive {
        // No `track_focus` here. The dialog host already owns this card's
        // focus trap and tab group; a second focus container inside it would
        // add a stop with no control on it, and shift-tab off the first field
        // would land on nothing. `on_key_down` needs only to be an ancestor of
        // whatever holds focus, which this is.
        frame = frame.on_key_down(move |event, _, cx| {
            let event = event.clone();
            keys.update(cx, |this, cx| this.venue_key(&event, cx));
        });
    }
    frame
        .child(header(state, route, mode, app, window))
        .child(div().flex_1().min_h_0().flex().child(match route {
            Route::Browse => browser(state, mode, app, window, cx),
            Route::Create => create(state),
        }))
        .child(footer(route))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Bands
// ---------------------------------------------------------------------------

fn header(
    state: &VenuePicker,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let interactive = mode == ContentMode::Interactive;
    let creating = route == Route::Create;
    let mut band = float::header_band();

    // Back exists only where there is somewhere to go: the browse route is
    // the root of this dialog.
    if creating && !state.venues.is_empty() {
        let cancel = app.clone();
        let pressable = interactive && !state.creating;
        let cap = float::key_cap();
        let cap = if pressable {
            float::key_cap_pressable(cap)
        } else {
            cap
        };
        band = band.child(
            cap.id("cancel-create-venue")
                .when(interactive, |cap| {
                    cap.track_focus(&state.leading_focus).tab_index(0)
                })
                .when(pressable, |cap| {
                    cap.on_click(move |_, _, cx| {
                        cancel.update(cx, |this, cx| this.cancel_create_venue(cx))
                    })
                })
                .child(gpui_component::Icon::new(IconName::ArrowLeft).size(px(12.5)))
                .agent_node(Role::Button, "Cancel")
                .agent_disabled(!interactive || state.creating)
                .agent_focused(interactive && state.leading_focus.is_focused(window)),
        );
    }

    band = band.child(field(state, route, mode, window));

    if let Some(chip) = create_chip(state, route, mode, app, window) {
        band = band.child(chip);
    }

    let close = app.clone();
    let cap = if interactive {
        float::key_cap_pressable(float::key_cap())
    } else {
        float::key_cap()
    };
    band.child(
        cap.id("close-venues")
            .when(interactive, |cap| {
                cap.track_focus(&state.close_focus)
                    .tab_index(0)
                    .on_click(move |_, _, cx| close.update(cx, |this, cx| this.dismiss_overlay(cx)))
            })
            .child("esc")
            .agent_node(Role::Button, "Close")
            .agent_disabled(!interactive)
            .agent_focused(interactive && state.close_focus.is_focused(window)),
    )
}

/// The header's one field: the venue filter on browse, the new venue's name on
/// create. Same slot, because it is the same question one step apart — "which
/// room?" and "what is this room called?".
fn field(state: &VenuePicker, route: Route, mode: ContentMode, window: &Window) -> AnyElement {
    let interactive = mode == ContentMode::Interactive;
    // The field is its own tab stop (see `TextInput`); this slot only sizes it.
    let slot = div().flex_1().min_w_0().text_size(px(14.0));
    // The semantic label is the VALUE once there is one, falling back to the
    // placeholder. A driver asserting on this field is asking what it says,
    // not what it would say if empty.
    let (entity, focus, value, placeholder) = match route {
        Route::Browse => (
            &state.search,
            &state.search_focus,
            state.query.as_str(),
            "Search venues…",
        ),
        Route::Create => (
            &state.name,
            &state.create_focus,
            state.create_name.as_str(),
            "Venue name",
        ),
    };
    let label = if value.is_empty() { placeholder } else { value };
    if !interactive {
        // An in-flight copy paints the text; a live field would register a
        // focus handle, which the morph contract forbids of a paint-only layer.
        return slot
            .text_color(if value.is_empty() {
                ladder::muted_foreground().into()
            } else {
                ladder::foreground_alpha(1.0)
            })
            .child(label.to_string())
            .agent_node(Role::Input, label.to_string())
            .agent_disabled(true)
            .into_any_element();
    }
    slot.child(entity.clone())
        .agent_node(Role::Input, label.to_string())
        .agent_focused(focus.is_focused(window))
        .into_any_element()
}

/// The committing chip. On browse it opens the create route; on create it
/// submits. One label, because from where the user sits it is one intent.
fn create_chip(
    state: &VenuePicker,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> Option<AnyElement> {
    if route == Route::Browse && (!state.loaded || state.error.is_some()) {
        return None;
    }
    let creating = route == Route::Create;
    let interactive = mode == ContentMode::Interactive;
    let label = if state.creating {
        "Creating…"
    } else {
        "Create venue"
    };
    let enabled = if creating {
        !state.create_name.trim().is_empty() && !state.creating
    } else {
        true
    };
    let go = app.clone();
    Some(
        float::btn_primary_chip()
            .id("submit-create-venue")
            .when(enabled && interactive, |chip| {
                chip.track_focus(&state.create_action_focus).tab_index(0)
            })
            .when(!enabled || !interactive, |chip| {
                chip.opacity(float::INERT_OPACITY)
            })
            .when(enabled && interactive, |chip| {
                chip.on_click(move |_, _, cx| {
                    go.update(cx, |this, cx| {
                        if creating {
                            this.create_venue(cx)
                        } else {
                            this.show_create_venue(cx)
                        }
                    })
                })
            })
            // The glyph must match the chord the footer legend promises: on
            // browse ⌘↵ opens this route, on create ↵ alone commits it.
            .child(if creating { "↵" } else { "⌘↵" })
            .child(label)
            .agent_node(Role::Button, label)
            .agent_disabled(!enabled || !interactive)
            .agent_focused(interactive && state.create_action_focus.is_focused(window))
            .into_any_element(),
    )
}

fn footer(route: Route) -> Div {
    let mut band = float::footer_band();
    if route == Route::Browse {
        band = band
            .child(float::key_hint_pair(
                IconName::ArrowUp,
                IconName::ArrowDown,
                "Navigate",
            ))
            .child(float::key_hint(IconName::ArrowRight, "Open"))
            .child(float::key_hint_text("⌘↵", "Create"));
    } else {
        band = band
            .child(float::key_hint(IconName::ArrowLeft, "Back"))
            .child(float::key_hint_text("↵", "Create"));
    }
    band.child(div().flex_1().min_w_0())
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

fn browser(
    state: &VenuePicker,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
    cx: &mut gpui::App,
) -> AnyElement {
    match &state.error {
        Some(error) => venue_error(error, app),
        None if !state.loaded => float::viewport()
            .child(
                float::list().child(
                    float::skeleton_rows(5, app.entity_id(), cx)
                        .agent_node(Role::Text, "Loading venues…"),
                ),
            )
            .into_any_element(),
        None if state.shown.is_empty() => float::viewport()
            .child(float::list().child(
                float::empty_row("No matching venues").agent_node(Role::Text, "No matching venues"),
            ))
            .into_any_element(),
        None => venue_list(state, mode, app, window),
    }
}

/// The venue list.
///
/// A plain tracked column rather than a virtualized list: a catalogue is tens
/// of venues, not thousands, and `uniform_list` renders only what is on screen
/// — which silently drops the off-screen rows out of the dialog's tab ring.
/// Every venue must stay reachable by keyboard, so every venue is built.
fn venue_list(
    state: &VenuePicker,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    // The gutters live on `float::viewport`, OUTSIDE the scroller — vertical
    // padding inside a scroll container is eaten twice (see `float::viewport`),
    // which pinned the last venue flush to the footer under `scroll_to_item`.
    float::viewport()
        .child(
            float::list()
                .id("venue-list")
                .overflow_y_scroll()
                .track_scroll(&state.list_scroll)
                .children(state.shown.iter().enumerate().map(|(index, venue)| {
                    let focus = state
                        .venue_focuses
                        .get(&venue.id)
                        .expect("loaded venue has no focus handle");
                    venue_card(venue, index == state.active, mode, focus, app, window)
                })),
        )
        .into_any_element()
}

fn venue_card(
    venue: &Venue,
    cursor: bool,
    mode: ContentMode,
    focus: &FocusHandle,
    app: &Entity<Luma>,
    window: &Window,
) -> impl IntoElement {
    let interactive = mode == ContentMode::Interactive;
    let id = venue.id.clone();
    let opened = app.clone();
    let key = format!("venue-{id}");
    float::menu_row(RowState::of(false, cursor), key)
        .id(ElementId::Name(SharedString::from(id.clone())))
        .when(interactive, |row| row.track_focus(focus).tab_index(0))
        .w_full()
        .h(px(CARD_HEIGHT))
        .px(px(10.0))
        .gap(px(12.0))
        .when(interactive, |row| {
            row.on_click(move |_, _, cx| {
                opened.update(cx, |this, cx| this.open_picker_venue(&id, cx))
            })
        })
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
                        .child(venue.name.clone()),
                )
                .when_some(venue.description.clone(), |column, description| {
                    column.child(
                        div()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(ladder::muted_foreground())
                            .child(description),
                    )
                }),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(11.0))
                .text_color(ladder::muted_foreground())
                .child(local_date(&venue.updated_at)),
        )
        .agent_node(Role::Card, venue.name.clone())
        .agent_disabled(!interactive)
        .agent_focused(interactive && focus.is_focused(window))
}

/// The load failure, with its own clipped viewport.
///
/// A backend can hand back thousands of lines or one enormous token, and gpui
/// wraps at whitespace with no break-anywhere mode — so the visual surface is
/// a bounded ellipsis while the semantic node keeps the complete error for
/// anything reading the tree.
fn venue_error(error: &str, app: &Entity<Luma>) -> AnyElement {
    let retry = app.clone();
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.0))
        .px(px(40.0))
        .child(
            div()
                .w_full()
                .max_w(px(520.0))
                .h(px(36.0))
                .overflow_hidden()
                .flex()
                .items_center()
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_center()
                        .text_color(ladder::danger())
                        .child(error.to_string())
                        .agent_node(Role::Text, error.to_string()),
                )
                .agent_node(Role::Card, "Venue error viewport"),
        )
        .child(
            float::btn("Retry", "venue-retry")
                .id("retry-venues")
                .on_click(move |_, _, cx| retry.update(cx, |this, cx| this.show_venues(cx)))
                .agent_node(Role::Button, "Retry"),
        )
        .into_any_element()
}

/// The create route's body: the prompt above, whatever went wrong below. The
/// name itself is typed in the header band — see [`field`].
fn create(state: &VenuePicker) -> AnyElement {
    let heading = if state.venues.is_empty() {
        "Create your first venue"
    } else {
        "A new room to light"
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .px(px(40.0))
        .child(
            div()
                .text_size(px(22.0))
                .font_weight(FontWeight::LIGHT)
                .child(heading)
                .agent_node(Role::Text, heading),
        )
        .child(
            div()
                .text_size(px(12.5))
                .text_color(ladder::muted_foreground())
                .child("Name it in the field above, then press ⌘↵."),
        )
        .when_some(state.create_error.clone(), |column, error| {
            column.child(
                div()
                    .max_w(px(520.0))
                    .truncate()
                    .text_color(ladder::danger())
                    .child(error.clone())
                    .agent_node(Role::Text, error),
            )
        })
        .into_any_element()
}

/// How long ago `timestamp` (ISO-8601, UTC) was, as a list-column age:
/// "now", "5m", "2h", "3d", "2w", "4mo", "1y". One unit, no "ago" — a column
/// of these reads at a glance where a column of dates has to be parsed.
pub(crate) fn relative_age(timestamp: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return local_date(timestamp);
    };
    let elapsed = chrono::Utc::now().signed_duration_since(then);
    let minutes = elapsed.num_minutes().max(0);
    let hours = minutes / 60;
    let days = hours / 24;
    if minutes < 1 {
        "now".into()
    } else if hours < 1 {
        format!("{minutes}m")
    } else if days < 1 {
        format!("{hours}h")
    } else if days < 14 {
        format!("{days}d")
    } else if days < 60 {
        format!("{}w", days / 7)
    } else if days < 365 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    }
}

pub(crate) fn local_date(timestamp: &str) -> String {
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
