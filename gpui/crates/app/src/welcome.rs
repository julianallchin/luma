//! The venue chooser, first-run create flow, and venue invites.
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
//! Every route puts its field in the header: on Browse it filters venues, on
//! Create it *is* the new venue's name, on Join it is the invite code. A route
//! whose header emptied out on the way to creating something would make the
//! routes feel like separate screens rather than one picker with modes.
//!
//! # Invites
//!
//! Share is the owner's view of one venue: its invite code with copy, replace
//! and revoke, and everyone who can open it. Join redeems a code. Both are
//! routes of this one card rather than dialogs of their own, because "who can
//! open this room" and "open a room someone gave me" are questions about the
//! catalogue this card already is.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_lib::models::venues::{display_share_code, Venue, VenueMember, ROLE_OWNER};

use crate::agent::ChatStatus;
use crate::library::LibraryError;
use luma_ui::dialog::morph::{self, ContentMode, MorphDialog, MorphSize, RouteDescriptor};
use luma_ui::float::{self, RowState};
use luma_ui::icons::IconName;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::text_input::{self, TextInput};
use luma_ui::{glass, ladder, radius};

use crate::{shell::Overlay, Luma};

pub(crate) const LAST_VENUE: &str = "last-venue";
/// The one size every route takes. Like the track palette, the routes are the
/// same object a step apart, so the card never resizes and the morph spends
/// its whole span on the content instead of the frame.
const PICKER_SIZE: MorphSize = MorphSize::new(680.0, 460.0);
/// Venue rows are taller than a track row: a venue carries a name, a
/// description and a date, and the row is the primary object on this screen
/// rather than one of hundreds.
const CARD_HEIGHT: f32 = 54.;
/// The example in every place a code is described, so the placeholder and the
/// prose cannot disagree about what a code looks like.
const EXAMPLE_CODE: &str = "K7M4-Q9TX";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Browse,
    Create,
    Join,
    Share,
}

impl Route {
    fn descriptor(self) -> RouteDescriptor<Self> {
        RouteDescriptor::exact(self, PICKER_SIZE.width, PICKER_SIZE.height)
            .with_transition(morph::direction(self != Self::Browse))
    }
}

/// Which of the venue menu's actions opened the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Create,
    Join,
    /// Share the venue with this id.
    Share(String),
}

impl Entry {
    fn route(&self) -> Route {
        match self {
            Self::Create => Route::Create,
            Self::Join => Route::Join,
            Self::Share(_) => Route::Share,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusTarget {
    Search,
    CreateName,
    JoinCode,
    SharePrimary,
}

/// The Share route's subject: one venue this account owns.
struct Sharing {
    venue: Venue,
    /// `None` while the first read is in flight.
    members: Option<Result<Vec<VenueMember>, String>>,
    member_focuses: HashMap<String, FocusHandle>,
    /// A code write in flight; the code's buttons are inert until it lands.
    busy: bool,
    error: Option<String>,
    /// The code on screen was just copied. Cleared when the code changes.
    copied: bool,
    /// The member whose Remove was pressed once and now asks to be confirmed.
    confirm_remove: Option<String>,
}

impl Sharing {
    fn new(venue: Venue) -> Self {
        Self {
            venue,
            members: None,
            member_focuses: HashMap::new(),
            busy: false,
            error: None,
            copied: false,
            confirm_remove: None,
        }
    }
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
    join_code: String,
    join_error: Option<String>,
    joining: bool,
    sharing: Option<Sharing>,
    /// Where the title bar's venue menu sent this picker in, or none for the
    /// picker a window with no venue opens on. Back from the route it entered
    /// at closes the dialog: the menu is the list it came from.
    entry: Option<Entry>,
    /// The header band's fields — see the module docs on why every route
    /// puts one there.
    search: Entity<TextInput>,
    name: Entity<TextInput>,
    code: Entity<TextInput>,
    search_focus: FocusHandle,
    create_focus: FocusHandle,
    code_focus: FocusHandle,
    /// One handle per header control. A `tab_index` alone makes an element a
    /// stop that cannot *hold* focus — gpui then settles focus on the nearest
    /// focusable ancestor, which is the dialog container, and the ring reads as
    /// broken. Every stop needs a handle of its own.
    ///
    /// The Back cap of every route but Browse. Browse has nothing in that slot
    /// — patterns are reached from the tab strip's + menu, not from the venue
    /// switcher: picking a room and picking a light pattern are different
    /// questions, and only one of them is what this dialog is for.
    leading_focus: FocusHandle,
    /// The header's committing chip: Create on Browse and Create, Join on Join.
    create_action_focus: FocusHandle,
    join_entry_focus: FocusHandle,
    close_focus: FocusHandle,
    /// Share's first control: Copy when there is a code, Create when not.
    share_primary_focus: FocusHandle,
    share_rotate_focus: FocusHandle,
    share_revoke_focus: FocusHandle,
    venue_focuses: HashMap<String, FocusHandle>,
    /// The Share chip on each owned venue's row.
    share_focuses: HashMap<String, FocusHandle>,
    focus_pending: Option<FocusTarget>,
    /// Which venue the arrow keys are on. Not a selection: opening a venue is
    /// the only thing this list does, so there is no state to keep.
    active: usize,
    list_scroll: ScrollHandle,
    _field_subscriptions: [Subscription; 3],
}

impl VenuePicker {
    fn loading(generation: u64, cx: &mut Context<Luma>) -> Self {
        let search = cx.new(|cx| TextInput::search("Search venues…", cx));
        let name = cx.new(|cx| TextInput::search("Venue name", cx));
        let code = cx.new(|cx| TextInput::search(format!("Invite code, like {EXAMPLE_CODE}"), cx));
        let search_focus = search.read(cx).focus_handle(cx);
        let create_focus = name.read(cx).focus_handle(cx);
        let code_focus = code.read(cx).focus_handle(cx);
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
            cx.subscribe(&code, |luma, field, event, cx| {
                if event == &text_input::Event::Edited {
                    let code = field.read(cx).text().to_string();
                    luma.join_code_changed(code, cx);
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
            join_code: String::new(),
            join_error: None,
            joining: false,
            sharing: None,
            entry: None,
            search,
            name,
            code,
            search_focus,
            create_focus,
            code_focus,
            leading_focus: cx.focus_handle().tab_stop(true),
            create_action_focus: cx.focus_handle().tab_stop(true),
            join_entry_focus: cx.focus_handle().tab_stop(true),
            close_focus: cx.focus_handle().tab_stop(true),
            share_primary_focus: cx.focus_handle().tab_stop(true),
            share_rotate_focus: cx.focus_handle().tab_stop(true),
            share_revoke_focus: cx.focus_handle().tab_stop(true),
            venue_focuses: HashMap::new(),
            share_focuses: HashMap::new(),
            focus_pending: None,
            active: 0,
            list_scroll: ScrollHandle::new(),
            _field_subscriptions: subscriptions,
        }
    }

    fn finish(&mut self, venues: Vec<Venue>, cx: &mut Context<Luma>) {
        self.adopt(venues, cx);
        self.loaded = true;
        self.error = None;
        let shared = match &self.entry {
            Some(Entry::Share(id)) => self
                .venues
                .iter()
                .find(|venue| &venue.id == id && shareable(venue))
                .cloned(),
            _ => None,
        };
        if let Some(venue) = shared {
            self.sharing = Some(Sharing::new(venue));
            self.settle_route(Route::Share);
            self.focus_pending = Some(FocusTarget::SharePrimary);
        } else if self.entry == Some(Entry::Join) {
            self.settle_route(Route::Join);
            self.focus_pending = Some(FocusTarget::JoinCode);
        } else if self.entry == Some(Entry::Create) || self.venues.is_empty() {
            self.settle_route(Route::Create);
            self.focus_pending = Some(FocusTarget::CreateName);
        } else {
            self.settle_route(Route::Browse);
            self.focus_pending = Some(FocusTarget::Search);
        }
        self.refilter();
    }

    /// Take a fresh catalogue without moving: the route, the query and the
    /// venue being shared all stay. Rows that arrive by download land here.
    fn refresh(&mut self, venues: Vec<Venue>, cx: &mut Context<Luma>) {
        let active = self.shown.get(self.active).map(|venue| venue.id.clone());
        self.adopt(venues, cx);
        self.refilter();
        if let Some(index) =
            active.and_then(|id| self.shown.iter().position(|venue| venue.id == id))
        {
            self.active = index;
        }
        if let Some(sharing) = self.sharing.as_mut() {
            if let Some(venue) = self
                .venues
                .iter()
                .find(|venue| venue.id == sharing.venue.id)
            {
                if venue.share_code != sharing.venue.share_code {
                    sharing.copied = false;
                }
                sharing.venue = venue.clone();
            }
        }
    }

    fn adopt(&mut self, venues: Vec<Venue>, cx: &mut Context<Luma>) {
        let mut old = std::mem::take(&mut self.venue_focuses);
        let mut old_share = std::mem::take(&mut self.share_focuses);
        for venue in &venues {
            let focus = old
                .remove(&venue.id)
                .unwrap_or_else(|| cx.focus_handle().tab_stop(true));
            self.venue_focuses.insert(venue.id.clone(), focus);
            if shareable(venue) {
                let focus = old_share
                    .remove(&venue.id)
                    .unwrap_or_else(|| cx.focus_handle().tab_stop(true));
                self.share_focuses.insert(venue.id.clone(), focus);
            }
        }
        self.venues = venues.into();
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

    /// Where Back goes from Join: the list, or — for someone with no venue
    /// yet — the create route they came from.
    fn join_return(&self) -> Route {
        if self.venues.is_empty() {
            Route::Create
        } else {
            Route::Browse
        }
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

/// Whether this account may invite people into `venue`: it owns it, and it
/// is an account's venue rather than the signed-out guest's.
fn shareable(venue: &Venue) -> bool {
    venue.is_owner() && venue.uid.is_some()
}

/// What a command's refusal says to a person: the seam's own prose, without
/// the wire name the `Display` of a [`LibraryError`] leads with.
fn prose(error: &LibraryError) -> String {
    error
        .command()
        .map(ToString::to_string)
        .unwrap_or_else(|| error.to_string())
}

/// A member as the owner sees them. The server keeps no profile another
/// account may read, so a member is the start of their account id.
fn member_name(member: &VenueMember) -> String {
    if member.is_you {
        "You".into()
    } else {
        format!("Account {}", member.uid.chars().take(8).collect::<String>())
    }
}

impl Luma {
    fn next_venue_picker_generation(&mut self) -> u64 {
        self.venue_picker_generation = self.venue_picker_generation.wrapping_add(1);
        self.venue_picker_generation
    }

    /// Restore behind the startup splash; only reveal the picker when a
    /// choice or an error needs the user’s attention.
    pub(crate) fn restore_venue(&mut self, cx: &mut Context<Self>) {
        self.restoring_venue = true;
        let generation = self.next_venue_picker_generation();
        self.overlay
            .open(Overlay::Venues(Box::new(VenuePicker::loading(
                generation, cx,
            ))));
        let venues = self.library.venues();
        let remembered = self.library.get_session_item(LAST_VENUE);
        let saved_venues = self.read_saved_venues();
        let saved_cameras = self.read_saved_cameras();
        let deck_width = self.library.get_session_item(crate::shell::DECK_WIDTH_KEY);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let venues = venues.await;
            let remembered = remembered.await;
            let saved_venues = saved_venues.await;
            let saved_cameras = saved_cameras.await;
            let deck_width = deck_width.await;
            this.update(cx, |this, cx| {
                this.restoring_venue = false;
                // Every venue's mode and score, restored as each venue is
                // opened. A store that cannot be read is nothing saved.
                this.saved_venues =
                    crate::saved_venues::SavedVenues::read(saved_venues.ok().flatten().as_deref());
                // The same for each venue's last camera pose.
                this.saved_cameras =
                    crate::saved_cameras::read(saved_cameras.ok().flatten().as_deref());
                this.deck_width = crate::session_store::Stored::read(
                    crate::shell::DECK_WIDTH_KEY,
                    deck_width.ok().flatten().as_deref(),
                );
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
                    this.overlay = luma_ui::dialog::Popup::default();
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

    /// The picker a window with no venue opens on: the catalogue, or the
    /// create route when there is none yet.
    pub(crate) fn show_venues(&mut self, cx: &mut Context<Self>) {
        self.show_venue_dialog(None, cx);
    }

    /// Open the picker with a freshly correlated catalogue read, at the route
    /// `entry` names — the venue menu's New venue, Join with code and Share.
    pub(crate) fn show_venue_dialog(&mut self, entry: Option<Entry>, cx: &mut Context<Self>) {
        let generation = self.next_venue_picker_generation();
        let mut picker = VenuePicker::loading(generation, cx);
        picker.entry = entry.clone();
        self.overlay.open(Overlay::Venues(Box::new(picker)));
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
                let sharing = state
                    .sharing
                    .as_ref()
                    .map(|sharing| sharing.venue.id.clone());
                cx.notify();
                if let Some(venue_id) = sharing {
                    this.load_share_members(venue_id, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Rows changed under an open picker — a download, or this device's own
    /// sharing write coming back. Re-read the catalogue in place: rebuilding
    /// the picker would throw away the route and whatever was typed, and a
    /// sync arriving mid-join would bounce the person back to the list.
    pub(crate) fn refresh_venues(&mut self, cx: &mut Context<Self>) {
        let (generation, sharing) = match self.overlay.as_open() {
            Some(Overlay::Venues(state)) if state.loaded && state.error.is_none() => (
                state.generation,
                state
                    .sharing
                    .as_ref()
                    .filter(|_| state.route() == Route::Share)
                    .map(|sharing| sharing.venue.id.clone()),
            ),
            Some(Overlay::Venues(_)) => return self.show_venues(cx),
            _ => return,
        };
        let pending = self.library.venues();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let (Some(Overlay::Venues(state)), Ok(rows)) = (this.overlay.open_mut(), result)
                {
                    if state.generation == generation {
                        state.refresh(rows, cx);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
        if let Some(venue_id) = sharing {
            self.load_share_members(venue_id, cx);
        }
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

    fn join_code_changed(&mut self, code: String, cx: &mut Context<Self>) {
        self.with_venue_picker(cx, |state| {
            state.join_code = code;
            state.join_error = None;
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
            Route::Join => state.join_code.is_empty(),
            Route::Share => true,
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
            (Route::Create | Route::Browse, "j") if modified => self.show_join_venue(cx),
            // A code is edited, so ← only leaves an empty field.
            (Route::Join, "left" | "backspace") if field_empty => self.cancel_join_venue(cx),
            (Route::Join, "enter") => self.join_venue(cx),
            (Route::Share, "left" | "backspace") => self.close_share_venue(cx),
            (Route::Share, "c") if modified => self.copy_share_code(cx),
            (Route::Browse, "up") => self.step_venue(-1, cx),
            (Route::Browse, "down") => self.step_venue(1, cx),
            (Route::Browse, "i") if modified => self.share_active_venue(cx),
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
        if self.leave_entry(Route::Create, cx) {
            return;
        }
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
                    // A new venue has no rig yet, so it starts on its setup tab.
                    Ok(venue) => {
                        this.open_venue(venue, cx);
                        this.open_rig(None, cx);
                    }
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

    // -----------------------------------------------------------------------
    // Join
    // -----------------------------------------------------------------------

    fn show_join_venue(&mut self, cx: &mut Context<Self>) {
        let reduced = luma_ui::motion::reduced_motion(cx);
        self.with_venue_picker(cx, move |state| {
            if !state.loaded || state.error.is_some() {
                return;
            }
            state.go(Route::Join, reduced);
            state.join_error = None;
            state.focus_pending = Some(FocusTarget::JoinCode);
        });
    }

    fn cancel_join_venue(&mut self, cx: &mut Context<Self>) {
        if self.leave_entry(Route::Join, cx) {
            return;
        }
        let reduced = luma_ui::motion::reduced_motion(cx);
        self.with_venue_picker(cx, move |state| {
            if state.joining {
                return;
            }
            let back = state.join_return();
            state.go(back, reduced);
            state.focus_pending = Some(if back == Route::Browse {
                FocusTarget::Search
            } else {
                FocusTarget::CreateName
            });
        });
    }

    fn join_venue(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::Venues(state)) = self.overlay.open_mut() else {
            return;
        };
        let code = state.join_code.trim().to_string();
        if code.is_empty() || state.joining {
            return;
        }
        state.joining = true;
        state.join_error = None;
        let generation = state.generation;
        let pending = self.library.join_venue(&code);
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
                    Ok(venue) => this.open_venue(venue, cx),
                    Err(error) => {
                        state.joining = false;
                        state.join_error = Some(prose(&error));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    // -----------------------------------------------------------------------
    // Share
    // -----------------------------------------------------------------------

    fn share_active_venue(&mut self, cx: &mut Context<Self>) {
        let id = match self.overlay.as_open() {
            Some(Overlay::Venues(state)) => state
                .shown
                .get(state.active)
                .filter(|venue| shareable(venue))
                .map(|venue| venue.id.clone()),
            _ => None,
        };
        if let Some(id) = id {
            self.show_share_venue(&id, cx);
        }
    }

    fn show_share_venue(&mut self, venue_id: &str, cx: &mut Context<Self>) {
        let reduced = luma_ui::motion::reduced_motion(cx);
        let mut opened = false;
        self.with_venue_picker(cx, |state| {
            let Some(venue) = state
                .venues
                .iter()
                .find(|venue| venue.id == venue_id && shareable(venue))
                .cloned()
            else {
                return;
            };
            state.sharing = Some(Sharing::new(venue));
            state.go(Route::Share, reduced);
            state.focus_pending = Some(FocusTarget::SharePrimary);
            opened = true;
        });
        if opened {
            self.load_share_members(venue_id.to_owned(), cx);
        }
    }

    fn close_share_venue(&mut self, cx: &mut Context<Self>) {
        if self.leave_entry(Route::Share, cx) {
            return;
        }
        let reduced = luma_ui::motion::reduced_motion(cx);
        self.with_venue_picker(cx, move |state| {
            state.go(Route::Browse, reduced);
            state.focus_pending = Some(FocusTarget::Search);
        });
    }

    fn load_share_members(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let Some(Overlay::Venues(state)) = self.overlay.as_open() else {
            return;
        };
        let generation = state.generation;
        let pending = self.library.venue_members(&venue_id);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let Some(Overlay::Venues(state)) = this.overlay.open_mut() else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                let Some(sharing) = state
                    .sharing
                    .as_mut()
                    .filter(|sharing| sharing.venue.id == venue_id)
                else {
                    return;
                };
                if let Ok(members) = &result {
                    let mut old = std::mem::take(&mut sharing.member_focuses);
                    for member in members.iter().filter(|member| !member.is_you) {
                        let focus = old
                            .remove(&member.uid)
                            .unwrap_or_else(|| cx.focus_handle().tab_stop(true));
                        sharing.member_focuses.insert(member.uid.clone(), focus);
                    }
                }
                sharing.members = Some(result.map_err(|error| prose(&error)));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Mint a code, or replace the one there is.
    fn rotate_share_code(&mut self, cx: &mut Context<Self>) {
        let Some(venue_id) = self.begin_share_write() else {
            return;
        };
        let pending = self.library.rotate_share_code(&venue_id);
        self.finish_share_write(venue_id, async move { pending.await.map(Some) }, cx);
    }

    fn revoke_share_code(&mut self, cx: &mut Context<Self>) {
        let Some(venue_id) = self.begin_share_write() else {
            return;
        };
        let pending = self.library.revoke_share_code(&venue_id);
        self.finish_share_write(venue_id, async move { pending.await.map(|()| None) }, cx);
    }

    /// Mark the code busy and name the venue, unless a write is in flight.
    fn begin_share_write(&mut self) -> Option<String> {
        let Some(Overlay::Venues(state)) = self.overlay.open_mut() else {
            return None;
        };
        let sharing = state.sharing.as_mut().filter(|sharing| !sharing.busy)?;
        sharing.busy = true;
        sharing.error = None;
        sharing.confirm_remove = None;
        Some(sharing.venue.id.clone())
    }

    fn finish_share_write(
        &mut self,
        venue_id: String,
        pending: impl std::future::Future<Output = Result<Option<String>, LibraryError>> + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(Overlay::Venues(state)) = self.overlay.as_open() else {
            return;
        };
        let generation = state.generation;
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
                let Some(sharing) = state
                    .sharing
                    .as_mut()
                    .filter(|sharing| sharing.venue.id == venue_id)
                else {
                    return;
                };
                sharing.busy = false;
                match result {
                    Ok(code) => {
                        sharing.venue.share_code = code.clone();
                        sharing.copied = false;
                        // The list's copy of the venue too, so Back shows the
                        // same code the next Share will.
                        state.venues = state
                            .venues
                            .iter()
                            .cloned()
                            .map(|mut venue| {
                                if venue.id == venue_id {
                                    venue.share_code = code.clone();
                                }
                                venue
                            })
                            .collect::<Vec<_>>()
                            .into();
                    }
                    Err(error) => sharing.error = Some(prose(&error)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn copy_share_code(&mut self, cx: &mut Context<Self>) {
        let code = match self.overlay.as_open() {
            Some(Overlay::Venues(state)) => state
                .sharing
                .as_ref()
                .and_then(|sharing| sharing.venue.share_code.as_deref())
                .map(display_share_code),
            _ => None,
        };
        let Some(code) = code else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(code));
        self.with_venue_picker(cx, |state| {
            if let Some(sharing) = state.sharing.as_mut() {
                sharing.copied = true;
            }
        });
    }

    /// The first press asks; the second, on the same member, removes.
    fn remove_venue_member(&mut self, uid: &str, cx: &mut Context<Self>) {
        let Some(Overlay::Venues(state)) = self.overlay.open_mut() else {
            return;
        };
        let generation = state.generation;
        let Some(sharing) = state.sharing.as_mut() else {
            return;
        };
        if sharing.confirm_remove.as_deref() != Some(uid) {
            sharing.confirm_remove = Some(uid.to_owned());
            cx.notify();
            return;
        }
        sharing.confirm_remove = None;
        sharing.error = None;
        let venue_id = sharing.venue.id.clone();
        let pending = self.library.remove_venue_member(&venue_id, uid);
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
                if let Err(error) = result {
                    if let Some(Overlay::Venues(state)) = this.overlay.open_mut() {
                        if let Some(sharing) = state.sharing.as_mut() {
                            sharing.error = Some(prose(&error));
                        }
                    }
                }
                this.load_share_members(venue_id, cx);
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

    /// Back from `route`, when the venue menu sent the picker straight there:
    /// the dialog closes rather than stepping back to a list the person never
    /// saw. Reports whether it did. A write in flight keeps the dialog.
    fn leave_entry(&mut self, route: Route, cx: &mut Context<Self>) -> bool {
        let leaving = matches!(
            self.overlay.as_open(),
            Some(Overlay::Venues(state)) if state.entry.as_ref().map(Entry::route) == Some(route)
                && !state.creating
                && !state.joining
        );
        if leaving {
            self.close_overlay(cx);
        }
        leaving
    }

    fn with_venue_picker(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut VenuePicker)) {
        if let Some(Overlay::Venues(state)) = self.overlay.open_mut() {
            edit(state);
            cx.notify();
        }
    }
}

// ---------------------------------------------------------------------------
// The title bar's venue menu
// ---------------------------------------------------------------------------

/// The venue menu: every venue to switch to, then the picker's other acts —
/// a new venue, joining one by code, sharing this one — and the account.
///
/// A menu rather than the picker's dialog, because switching venue is the
/// everyday act and a list one press deep is all it needs. The dialog stays
/// for what needs a field: the menu's acts open it straight at their route.
pub(crate) struct VenueMenu {
    generation: u64,
    /// The catalogue, read fresh on every opening; `None` while it is out.
    venues: Option<Result<Vec<Venue>, String>>,
}

impl Luma {
    /// Open or close the venue menu.
    pub(crate) fn toggle_venue_menu(&mut self, cx: &mut Context<Self>) {
        if self.venue_menu.is_open() {
            self.close_venue_menu(cx);
            return;
        }
        let generation = self.next_venue_picker_generation();
        self.venue_menu.open(VenueMenu {
            generation,
            venues: None,
        });
        let pending = self.library.venues();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Some(menu) = this.venue_menu.open_mut() {
                    if menu.generation == generation {
                        menu.venues = Some(result.map_err(|error| error.to_string()));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Begin the menu's exit. Every row does this first: the gesture is over
    /// the moment one is pressed.
    pub(crate) fn close_venue_menu(&mut self, cx: &mut Context<Self>) {
        self.venue_menu.begin_close(cx);
        cx.notify();
    }

    /// Switch to the venue `id` names, from the menu's list.
    fn switch_venue(&mut self, id: &str, cx: &mut Context<Self>) {
        let venue = self
            .venue_menu
            .get()
            .and_then(|menu| menu.venues.as_ref())
            .and_then(|venues| venues.as_ref().ok())
            .and_then(|venues| venues.iter().find(|venue| venue.id == id).cloned());
        self.close_venue_menu(cx);
        let current = self
            .songs
            .as_ref()
            .map(|songs| songs.venue_id().to_string());
        if let Some(venue) = venue.filter(|venue| Some(&venue.id) != current.as_ref()) {
            self.open_venue(venue, cx);
        }
    }
}

/// The venue's name in the title bar: the menu's trigger, and the menu hung
/// off it. A muted stage glyph before the name marks it as the venue.
pub(crate) fn venue_trigger(app: &Luma, entity: &Entity<Luma>, cx: &App) -> AnyElement {
    let name = app.songs.as_ref().map_or_else(
        || "Venues".to_string(),
        |songs| songs.venue_name().to_string(),
    );
    let open = app.venue_menu.is_open();
    let toggled = entity.clone();
    float::nav_row(RowState::of(open, false), "venue-menu-trigger")
        .id("venue-menu-trigger")
        .flex_none()
        .relative()
        .px(px(10.))
        .text_size(px(13.))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| toggled.update(cx, |this, cx| this.toggle_venue_menu(cx)))
        // A quiet stage glyph says the name is the venue's, not a song's.
        .child(
            gpui_component::Icon::new(IconName::Stage)
                .size(px(14.))
                .text_color(glass::ink(glass::TEXT_TERTIARY)),
        )
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(glass::ink(0.95))
                .child(name.clone()),
        )
        .child(
            gpui_component::Icon::new(IconName::ChevronDown)
                .size(px(11.))
                .text_color(glass::ink(0.42)),
        )
        .children(venue_menu(app, entity, cx))
        .agent_node(Role::Button, name)
        .into_any_element()
}

/// How wide the venue menu opens.
const VENUE_MENU_WIDTH: f32 = 260.;

fn venue_menu(app: &Luma, entity: &Entity<Luma>, cx: &App) -> Option<AnyElement> {
    let menu = app.venue_menu.get()?;
    let closing = app.venue_menu.closing_since();
    let pressable = closing.is_none();
    let current = app.songs.as_ref().map(|songs| songs.venue_id().to_string());
    let statuses = app.venue_statuses(cx);
    let row = |key: String, label: SharedString| {
        float::menu_row(RowState::Rest, key.clone())
            .id(SharedString::from(key))
            .child(div().flex_1().min_w_0().truncate().child(label))
    };
    let mut card = float::popover_card()
        .w(px(VENUE_MENU_WIDTH))
        .child(menu_heading("Venues"));
    match &menu.venues {
        None => card = card.child(menu_note("Loading venues…")),
        Some(Err(error)) => {
            card = card.child(menu_note(format!("Failed to load venues: {error}")));
        }
        Some(Ok(venues)) => {
            for venue in venues {
                let here = current.as_deref() == Some(venue.id.as_str());
                let (picked, id) = (entity.clone(), venue.id.clone());
                card = card.child(
                    row(
                        format!("venue-menu:{}", venue.id),
                        venue.name.clone().into(),
                    )
                    .when(pressable, |row| {
                        row.on_click(move |_, _, cx| {
                            picked.update(cx, |this, cx| this.switch_venue(&id, cx))
                        })
                    })
                    .children(
                        statuses
                            .get(&venue.id)
                            .map(|status| crate::agent::status_dot(*status, &venue.name)),
                    )
                    // A venue someone shared says so, as the picker does.
                    .when(venue.is_member(), |row| {
                        row.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(glass::ink(glass::TEXT_TERTIARY))
                                .child("Shared with you")
                                .agent_node(
                                    Role::Text,
                                    format!("{} is shared with you", venue.name),
                                ),
                        )
                    })
                    .child(float::check(here))
                    .agent_node(Role::Row, venue.name.clone()),
                );
            }
        }
    }
    let share = menu
        .venues
        .as_ref()
        .and_then(|venues| venues.as_ref().ok())
        .and_then(|venues| {
            venues
                .iter()
                .find(|venue| current.as_deref() == Some(venue.id.as_str()))
        })
        .filter(|venue| shareable(venue))
        .map(|venue| (venue.id.clone(), venue.name.clone()));
    let act = |key: &'static str, label: SharedString, chord: Option<&'static str>, act: Act| {
        let pressed = entity.clone();
        row(key.to_string(), label.clone())
            .when(pressable, |row| {
                row.on_click(move |_, _, cx| {
                    let act = act.clone();
                    pressed.update(cx, |this, cx| {
                        this.close_venue_menu(cx);
                        match act {
                            Act::Dialog(entry) => this.show_venue_dialog(Some(entry), cx),
                            Act::Settings => this.open_settings(cx),
                            Act::Identity => this.switch_identity(cx),
                        }
                    })
                })
            })
            .children(chord.map(|chord| float::key_cap().child(chord)))
            .agent_node(Role::Row, label)
    };
    card = card
        .child(menu_rule())
        .child(act(
            "venue-menu-new",
            "New venue…".into(),
            None,
            Act::Dialog(Entry::Create),
        ))
        .child(act(
            "venue-menu-join",
            "Join with code…".into(),
            None,
            Act::Dialog(Entry::Join),
        ))
        .children(share.map(|(id, name)| {
            act(
                "venue-menu-share",
                format!("Share {name}…").into(),
                None,
                Act::Dialog(Entry::Share(id)),
            )
        }))
        .child(menu_rule())
        .child(act(
            "venue-menu-settings",
            "Settings…".into(),
            Some("⌘,"),
            Act::Settings,
        ))
        .child(act(
            "venue-menu-identity",
            if app.account_action.signing_out {
                "Signing out…".into()
            } else if app.library.user_id().is_some() {
                "Sign out".into()
            } else {
                "Sign in".into()
            },
            None,
            Act::Identity,
        ));
    let card = card.into_any_element();
    Some(match closing {
        Some(since) => float::anchored_below_closing(
            "venue-menu",
            float::NAV_ROW_HEIGHT,
            card,
            luma_ui::motion::exit_progress(&luma_ui::motion::MENU_OUT, since),
        ),
        None => {
            let dismiss = entity.clone();
            float::anchored_below(
                "venue-menu",
                float::NAV_ROW_HEIGHT,
                float::Dismiss::on_press_out(move |_, cx| {
                    dismiss.update(cx, |this, cx| this.close_venue_menu(cx));
                }),
                card,
            )
        }
    })
}

/// What a venue menu row below the list does.
#[derive(Clone)]
enum Act {
    Dialog(Entry),
    Settings,
    Identity,
}

/// The small heading over the menu's list.
fn menu_heading(text: &'static str) -> Div {
    div()
        .h(px(24.))
        .flex()
        .items_center()
        .px(px(float::ROW_INSET))
        .text_size(px(11.))
        .text_color(glass::ink(glass::TEXT_TERTIARY))
        .child(text)
}

/// A line the menu says instead of its list.
fn menu_note(text: impl Into<SharedString>) -> impl IntoElement {
    let text = text.into();
    div()
        .px(px(float::ROW_INSET))
        .py(px(6.))
        .text_size(px(12.))
        .text_color(glass::ink(glass::TEXT_SECONDARY))
        .child(text.clone())
        .agent_node(Role::Text, text)
}

/// The hairline between the menu's groups.
fn menu_rule() -> Div {
    div().my(px(4.)).h(px(1.)).bg(glass::inner_rule())
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
            Route::Join => FocusTarget::JoinCode,
            Route::Share => FocusTarget::SharePrimary,
        });
    }
    let Some(target) = state.focus_pending.take() else {
        return;
    };
    match target {
        FocusTarget::Search => window.focus(&state.search_focus, cx),
        FocusTarget::CreateName => window.focus(&state.create_focus, cx),
        FocusTarget::JoinCode => window.focus(&state.code_focus, cx),
        FocusTarget::SharePrimary => window.focus(&state.share_primary_focus, cx),
    }
}

/// The venue picker's card. Like the track palette, this owns its own
/// `morph::card` — the shell hands it no box to live in.
///
/// `statuses` are the other venues' status dots: a venue whose tabs are not
/// on screen still says when its agents are working or have finished.
pub(crate) fn render(
    state: &VenuePicker,
    statuses: &HashMap<String, ChatStatus>,
    app: &Entity<Luma>,
    window: &Window,
    cx: &mut gpui::App,
) -> AnyElement {
    let sample = state.morph.sample(Instant::now());
    let app = app.clone();
    morph::card(&sample, "Venue dialog", move |route, mode| {
        route_body(state, statuses, *route, mode, &app, window, cx)
    })
}

fn route_body(
    state: &VenuePicker,
    statuses: &HashMap<String, ChatStatus>,
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
            Route::Browse => browser(state, statuses, mode, app, window, cx),
            Route::Create => create(state, mode, app),
            Route::Join => join(state),
            Route::Share => share(state, mode, app, window),
        }))
        .child(footer(state, route))
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
    let mut band = float::header_band();

    // Back exists only where there is somewhere to go: the browse route is
    // the root of this dialog, and onboarding's create route has no list
    // behind it.
    let back: Option<(bool, fn(&mut Luma, &mut Context<Luma>))> = match route {
        Route::Browse => None,
        Route::Create => (!state.venues.is_empty()).then_some((
            !state.creating,
            Luma::cancel_create_venue as fn(&mut Luma, &mut Context<Luma>),
        )),
        Route::Join => Some((!state.joining, Luma::cancel_join_venue)),
        Route::Share => Some((true, Luma::close_share_venue)),
    };
    if let Some((enabled, go_back)) = back {
        let cancel = app.clone();
        let pressable = interactive && enabled;
        let cap = float::key_cap();
        let cap = if pressable {
            float::key_cap_pressable(cap)
        } else {
            cap
        };
        band = band.child(
            cap.id("venue-dialog-back")
                .when(interactive, |cap| {
                    cap.track_focus(&state.leading_focus).tab_index(0)
                })
                .when(pressable, |cap| {
                    cap.on_click(move |_, _, cx| cancel.update(cx, |this, cx| go_back(this, cx)))
                })
                .child(gpui_component::Icon::new(IconName::ArrowLeft).size(px(12.5)))
                // Named for where it goes: the window's own Back is a button too.
                .agent_node(
                    Role::Button,
                    match route {
                        Route::Create => "Cancel",
                        Route::Share => "Back to venues",
                        _ => "Back",
                    },
                )
                .agent_disabled(!pressable)
                .agent_focused(interactive && state.leading_focus.is_focused(window)),
        );
    }

    band = band.child(match route {
        Route::Share => share_title(state),
        _ => field(state, route, mode, window),
    });

    if route == Route::Browse && state.loaded && state.error.is_none() {
        band = band.child(join_entry(state, interactive, app, window));
    }
    if let Some(chip) = commit_chip(state, route, mode, app, window) {
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
/// create, the invite code on join. Same slot, because it is the same question
/// one step apart — "which room?", "what is this room called?", "which room
/// were you given?".
fn field(state: &VenuePicker, route: Route, mode: ContentMode, window: &Window) -> AnyElement {
    let interactive = mode == ContentMode::Interactive;
    // The field is its own tab stop (see `TextInput`); this slot only sizes it.
    let slot = div().flex_1().min_w_0().text_size(px(14.0));
    // The semantic label is the VALUE once there is one, falling back to the
    // placeholder. A driver asserting on this field is asking what it says,
    // not what it would say if empty.
    let placeholder;
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
        Route::Join | Route::Share => {
            placeholder = format!("Invite code, like {EXAMPLE_CODE}");
            (
                &state.code,
                &state.code_focus,
                state.join_code.as_str(),
                placeholder.as_str(),
            )
        }
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

/// Share's header names the room, in the slot every other route puts its
/// field: the question this route answers is "who can open *this*?".
fn share_title(state: &VenuePicker) -> AnyElement {
    let name = state
        .sharing
        .as_ref()
        .map(|sharing| sharing.venue.name.clone())
        .unwrap_or_default();
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_row()
        .items_baseline()
        .gap(px(6.0))
        .text_size(px(14.0))
        .child(
            div()
                .flex_none()
                .text_color(ladder::muted_foreground())
                .child("Share"),
        )
        .child(
            div()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .child(name.clone()),
        )
        .agent_node(Role::Text, format!("Share {name}"))
        .into_any_element()
}

/// Browse's way to Join: a quiet chip beside the lit Create chip, because
/// joining is the less common of the two ways to get a venue.
fn join_entry(
    state: &VenuePicker,
    interactive: bool,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    let go = app.clone();
    quiet_chip()
        .id("show-join-venue")
        .when(interactive, |chip| {
            chip.track_focus(&state.join_entry_focus)
                .tab_index(0)
                .on_click(move |_, _, cx| go.update(cx, |this, cx| this.show_join_venue(cx)))
        })
        .child(
            div()
                .font_family(luma_ui::fonts::MONO)
                .text_size(px(11.0))
                .text_color(ladder::foreground_alpha(0.5))
                .child("⌘J"),
        )
        .child("Join")
        .agent_node(Role::Button, "Join a venue")
        .agent_disabled(!interactive)
        .agent_focused(interactive && state.join_entry_focus.is_focused(window))
        .into_any_element()
}

/// The unlit sibling of [`float::btn_primary_chip`]: cap height, cap radius,
/// a wash for a plate.
fn quiet_chip() -> Div {
    div()
        .h(px(float::KEY_CAP_HEIGHT))
        .px(px(8.0))
        .rounded(px(radius::CAP))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(5.0))
        .text_size(px(12.0))
        .text_color(ladder::foreground_alpha(0.85))
        .bg(glass::ink(0.06))
        .cursor_pointer()
        .hover(|style| style.bg(glass::ink(0.10)))
}

/// The committing chip. On browse it opens the create route; on create it
/// submits; on join it redeems the code. One label per intent, because from
/// where the user sits it is one intent.
fn commit_chip(
    state: &VenuePicker,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> Option<AnyElement> {
    let interactive = mode == ContentMode::Interactive;
    let (glyph, label, enabled, act): (_, _, _, fn(&mut Luma, &mut Context<Luma>)) = match route {
        Route::Browse if !state.loaded || state.error.is_some() => return None,
        Route::Browse => ("⌘↵", "Create venue", true, Luma::show_create_venue),
        Route::Create => (
            "↵",
            if state.creating {
                "Creating…"
            } else {
                "Create venue"
            },
            !state.create_name.trim().is_empty() && !state.creating,
            Luma::create_venue,
        ),
        Route::Join => (
            "↵",
            if state.joining { "Joining…" } else { "Join" },
            !state.join_code.trim().is_empty() && !state.joining,
            Luma::join_venue,
        ),
        Route::Share => return None,
    };
    let go = app.clone();
    Some(
        float::btn_primary_chip()
            .id("venue-dialog-commit")
            .when(enabled && interactive, |chip| {
                chip.track_focus(&state.create_action_focus).tab_index(0)
            })
            .when(!enabled || !interactive, |chip| {
                chip.opacity(float::INERT_OPACITY)
            })
            .when(enabled && interactive, |chip| {
                chip.on_click(move |_, _, cx| go.update(cx, |this, cx| act(this, cx)))
            })
            // The glyph must match the chord the footer legend promises.
            .child(glyph)
            .child(label)
            .agent_node(Role::Button, label)
            .agent_disabled(!enabled || !interactive)
            .agent_focused(interactive && state.create_action_focus.is_focused(window))
            .into_any_element(),
    )
}

fn footer(state: &VenuePicker, route: Route) -> Div {
    let mut band = float::footer_band();
    band = match route {
        Route::Browse => band
            .child(float::key_hint_pair(
                IconName::ArrowUp,
                IconName::ArrowDown,
                "Navigate",
            ))
            .child(float::key_hint(IconName::ArrowRight, "Open"))
            .child(float::key_hint_text("⌘↵", "Create"))
            .child(float::key_hint_text("⌘J", "Join"))
            .when(
                state.shown.get(state.active).is_some_and(shareable),
                |band| band.child(float::key_hint_text("⌘I", "Share")),
            ),
        Route::Create => band
            .when(!state.venues.is_empty(), |band| {
                band.child(float::key_hint(IconName::ArrowLeft, "Back"))
            })
            .child(float::key_hint_text("↵", "Create")),
        Route::Join => band
            .child(float::key_hint(IconName::ArrowLeft, "Back"))
            .child(float::key_hint_text("↵", "Join")),
        Route::Share => band
            .child(float::key_hint(IconName::ArrowLeft, "Back"))
            .when(
                state
                    .sharing
                    .as_ref()
                    .is_some_and(|sharing| sharing.venue.share_code.is_some()),
                |band| band.child(float::key_hint_text("⌘C", "Copy code")),
            ),
    };
    band.child(div().flex_1().min_w_0())
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

fn browser(
    state: &VenuePicker,
    statuses: &HashMap<String, ChatStatus>,
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
        None => venue_list(state, statuses, mode, app, window),
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
    statuses: &HashMap<String, ChatStatus>,
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
                    let status = statuses.get(&venue.id).copied();
                    venue_card(
                        venue,
                        index == state.active,
                        status,
                        mode,
                        focus,
                        state.share_focuses.get(&venue.id),
                        app,
                        window,
                    )
                })),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn venue_card(
    venue: &Venue,
    cursor: bool,
    status: Option<ChatStatus>,
    mode: ContentMode,
    focus: &FocusHandle,
    share_focus: Option<&FocusHandle>,
    app: &Entity<Luma>,
    window: &Window,
) -> impl IntoElement {
    let interactive = mode == ContentMode::Interactive;
    let id = venue.id.clone();
    let opened = app.clone();
    let key = format!("venue-{id}");
    let group = SharedString::from(format!("venue-row-{id}"));
    float::menu_row(RowState::of(false, cursor), key)
        .id(ElementId::Name(SharedString::from(id.clone())))
        .group(group.clone())
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
        .children(status.map(|status| crate::agent::status_dot(status, &venue.name)))
        // Whose room this is, said once: a venue someone shared carries the
        // word; a venue this account owns carries the way to share it, shown
        // where the pointer or the keyboard is so a list of rooms does not
        // read as a list of buttons.
        .when(venue.is_member(), |row| {
            row.child(
                div()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(ladder::muted_foreground())
                    .child("Shared with you")
                    .agent_node(Role::Text, format!("{} is shared with you", venue.name)),
            )
        })
        .when_some(share_focus, |row, share_focus| {
            let shared = app.clone();
            let id = venue.id.clone();
            let focused = interactive && share_focus.is_focused(window);
            row.child(
                quiet_chip()
                    .id(ElementId::Name(SharedString::from(format!("share-{id}"))))
                    .when(!(cursor || focused), |chip| {
                        chip.opacity(0.0)
                            .group_hover(group.clone(), |style| style.opacity(1.0))
                    })
                    .when(interactive, |chip| {
                        chip.track_focus(share_focus)
                            .tab_index(0)
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                shared.update(cx, |this, cx| this.show_share_venue(&id, cx))
                            })
                    })
                    .child("Share")
                    .agent_node(Role::Button, format!("Share {}", venue.name))
                    .agent_disabled(!interactive)
                    .agent_focused(focused),
            )
        })
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

/// A route's centred prompt: the heading, the line under it, then whatever
/// went wrong. Create and Join are the same screen asking different things.
fn prompt(heading: &str, line: &str, error: Option<String>) -> Div {
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
                .child(heading.to_string())
                .agent_node(Role::Text, heading.to_string()),
        )
        .child(
            div()
                .text_size(px(12.5))
                .text_color(ladder::muted_foreground())
                .child(line.to_string()),
        )
        .when_some(error, |column, error| {
            column.child(
                div()
                    .max_w(px(560.0))
                    .text_center()
                    .text_size(px(12.5))
                    .text_color(ladder::danger())
                    .child(error.clone())
                    .agent_node(Role::Text, error),
            )
        })
}

/// The create route's body: the prompt above, whatever went wrong below. The
/// name itself is typed in the header band — see [`field`].
///
/// Someone with no venue yet may have been sent a code rather than be here to
/// build a room, so onboarding offers the way to Join beneath the prompt.
fn create(state: &VenuePicker, mode: ContentMode, app: &Entity<Luma>) -> AnyElement {
    let onboarding = state.venues.is_empty();
    let heading = if onboarding {
        "Create your first venue"
    } else {
        "A new room to light"
    };
    let interactive = mode == ContentMode::Interactive;
    let go = app.clone();
    prompt(
        heading,
        "Name it in the field above, then press ↵.",
        state.create_error.clone(),
    )
    .when(onboarding && state.loaded, |column| {
        column.child(
            div().pt(px(6.0)).child(
                float::btn("Have an invite code? Join a venue", "onboarding-join")
                    .id("onboarding-join")
                    .when(interactive, |button| {
                        button.on_click(move |_, _, cx| {
                            go.update(cx, |this, cx| this.show_join_venue(cx))
                        })
                    })
                    .agent_node(Role::Button, "Join a venue")
                    .agent_disabled(!interactive),
            ),
        )
    })
    .into_any_element()
}

/// The join route's body. The code is typed in the header band.
fn join(state: &VenuePicker) -> AnyElement {
    prompt(
        "Join a venue",
        "Enter the invite code its owner shared with you, then press ↵.",
        state.join_error.clone(),
    )
    .into_any_element()
}

/// The share route's body: the code, then the people.
fn share(
    state: &VenuePicker,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    let Some(sharing) = state.sharing.as_ref() else {
        return div().into_any_element();
    };
    let interactive = mode == ContentMode::Interactive;
    div()
        .size_full()
        .flex()
        .flex_col()
        .px(px(16.0))
        .pt(px(16.0))
        .gap(px(18.0))
        .child(code_section(state, sharing, interactive, app, window))
        .child(people_section(sharing, interactive, app, window))
        .into_any_element()
}

fn code_section(
    state: &VenuePicker,
    sharing: &Sharing,
    interactive: bool,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let live = interactive && !sharing.busy;
    let action = |label: &'static str,
                  id: &'static str,
                  node: &'static str,
                  focus: &FocusHandle,
                  act: fn(&mut Luma, &mut Context<Luma>)| {
        let go = app.clone();
        float::btn(label, id)
            .id(id)
            .when(interactive, |button| button.track_focus(focus).tab_index(0))
            .when(live, |button| {
                button.on_click(move |_, _, cx| go.update(cx, |this, cx| act(this, cx)))
            })
            .when(!live, |button| button.opacity(float::INERT_OPACITY))
            .agent_node(Role::Button, node)
            .agent_disabled(!live)
            .agent_focused(interactive && focus.is_focused(window))
    };

    let plate = div()
        .flex_1()
        .min_w_0()
        .h(px(44.0))
        .px(px(14.0))
        .rounded(px(radius::PANEL))
        .bg(glass::ink(0.04))
        .border_1()
        .border_color(glass::hairline(0.06))
        .flex()
        .items_center();

    let row = div().flex().flex_row().items_center().gap(px(6.0));
    let row = match sharing.venue.share_code.as_deref() {
        Some(code) => {
            let shown = display_share_code(code);
            row.child(
                plate.child(
                    div()
                        .font_family(luma_ui::fonts::MONO)
                        .text_size(px(20.0))
                        .font_weight(FontWeight::MEDIUM)
                        .child(shown.clone())
                        .agent_node(Role::Text, shown),
                ),
            )
            .child(action(
                if sharing.copied { "Copied" } else { "Copy" },
                "copy-share-code",
                "Copy code",
                &state.share_primary_focus,
                Luma::copy_share_code,
            ))
            .child(action(
                "New code",
                "rotate-share-code",
                "New code",
                &state.share_rotate_focus,
                Luma::rotate_share_code,
            ))
            .child(action(
                "Revoke",
                "revoke-share-code",
                "Revoke code",
                &state.share_revoke_focus,
                Luma::revoke_share_code,
            ))
        }
        None => {
            let go = app.clone();
            row.child(
                plate.child(
                    div()
                        .text_size(px(13.0))
                        .text_color(ladder::muted_foreground())
                        .child("No invite code")
                        .agent_node(Role::Text, "No invite code"),
                ),
            )
            .child(
                float::btn_primary(if sharing.busy {
                    "Creating…"
                } else {
                    "Create invite code"
                })
                .id("create-share-code")
                .when(interactive, |button| {
                    button.track_focus(&state.share_primary_focus).tab_index(0)
                })
                .when(live, |button| {
                    button.on_click(move |_, _, cx| {
                        go.update(cx, |this, cx| this.rotate_share_code(cx))
                    })
                })
                .when(!live, |button| button.opacity(float::INERT_OPACITY))
                .agent_node(Role::Button, "Create invite code")
                .agent_disabled(!live)
                .agent_focused(interactive && state.share_primary_focus.is_focused(window)),
            )
        }
    };

    let note = if sharing.venue.share_code.is_some() {
        // Short enough for one line. What a new code or a revoke does to the
        // people already in (nothing) is said by the list below not changing.
        "Anyone with this code can open and edit this venue.".to_string()
    } else {
        "Create a code and send it to someone to let them open and edit this venue.".to_string()
    };

    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(float::label("Invite code").px(px(2.0)))
        .child(row)
        .child(
            div()
                .px(px(2.0))
                .text_size(px(12.0))
                .text_color(ladder::muted_foreground())
                .child(note),
        )
        .when_some(sharing.error.clone(), |column, error| {
            column.child(
                div()
                    .px(px(2.0))
                    .text_size(px(12.0))
                    .text_color(ladder::danger())
                    .child(error.clone())
                    .agent_node(Role::Text, error),
            )
        })
}

fn people_section(
    sharing: &Sharing,
    interactive: bool,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let heading = match &sharing.members {
        Some(Ok(members)) => format!("People with access · {}", members.len()),
        _ => "People with access".to_string(),
    };
    let rows: Vec<AnyElement> = match &sharing.members {
        None => vec![float::empty_row("Loading…").into_any_element()],
        Some(Err(error)) => vec![float::empty_row(error.clone())
            .text_color(ladder::danger())
            .agent_node(Role::Text, error.clone())
            .into_any_element()],
        Some(Ok(members)) => {
            let mut rows: Vec<AnyElement> = members
                .iter()
                .map(|member| {
                    member_row(
                        member,
                        sharing.member_focuses.get(&member.uid),
                        sharing.confirm_remove.as_deref() == Some(member.uid.as_str()),
                        interactive,
                        app,
                        window,
                    )
                })
                .collect();
            if members.iter().all(|member| member.is_you) {
                rows.push(
                    float::empty_row("Nobody else has access yet.")
                        .agent_node(Role::Text, "Nobody else has access yet.")
                        .into_any_element(),
                );
            }
            rows
        }
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(float::label(heading).px(px(2.0)))
        .child(
            div()
                .id("venue-members")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pb(px(8.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .children(rows),
        )
}

fn member_row(
    member: &VenueMember,
    focus: Option<&FocusHandle>,
    confirming: bool,
    interactive: bool,
    app: &Entity<Luma>,
    window: &Window,
) -> AnyElement {
    let name = member_name(member);
    let detail = if member.role == ROLE_OWNER {
        "Owner".to_string()
    } else {
        match relative_age(&member.since).as_str() {
            "now" => "Joined just now".to_string(),
            age => format!("Joined {age} ago"),
        }
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .h(px(40.0))
        .px(px(float::ROW_INSET))
        .rounded(px(radius::ROW))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(13.0))
                .when(!member.is_you, |name| {
                    name.font_family(luma_ui::fonts::MONO).text_size(px(12.0))
                })
                .child(name.clone()),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(11.5))
                .text_color(ladder::muted_foreground())
                .child(detail),
        )
        .when_some(focus, |row, focus| {
            let go = app.clone();
            let uid = member.uid.clone();
            let label = if confirming {
                "Remove access?"
            } else {
                "Remove"
            };
            row.child(
                float::btn(label, format!("remove-{uid}"))
                    .id(ElementId::Name(SharedString::from(format!("remove-{uid}"))))
                    .h(px(28.0))
                    .px(px(10.0))
                    .text_size(px(12.0))
                    .when(confirming, |button| button.text_color(ladder::danger()))
                    .when(interactive, |button| {
                        button
                            .track_focus(focus)
                            .tab_index(0)
                            .on_click(move |_, _, cx| {
                                go.update(cx, |this, cx| this.remove_venue_member(&uid, cx))
                            })
                    })
                    .agent_node(
                        Role::Button,
                        if confirming {
                            format!("Confirm removing {name}")
                        } else {
                            format!("Remove {name}")
                        },
                    )
                    .agent_disabled(!interactive)
                    .agent_focused(interactive && focus.is_focused(window)),
            )
        })
        .agent_node(Role::Row, name)
        .into_any_element()
}

/// How long ago `timestamp` (ISO-8601, UTC) was, as a list-column age:
/// "now", "5m", "2h", "3d", "2w", "4mo", "1y". One unit, no "ago" — a column
/// of these reads at a glance where a column of dates has to be parsed.
pub(crate) fn relative_age(timestamp: &str) -> String {
    let Some(then) = parse_utc(timestamp) else {
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

/// The two shapes the seam's timestamps come in.
///
/// Rows written by the app carry RFC 3339; rows whose column took SQLite's
/// `CURRENT_TIMESTAMP` default carry `YYYY-MM-DD HH:MM:SS`, which is UTC with
/// the offset left off. Reading only the first spells every score's age as a
/// date — which is how this was found.
fn parse_utc(timestamp: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone as _;
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(timestamp) {
        return Some(parsed.with_timezone(&chrono::Utc));
    }
    let naive = chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S").ok()?;
    chrono::Utc.from_local_datetime(&naive).single()
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
