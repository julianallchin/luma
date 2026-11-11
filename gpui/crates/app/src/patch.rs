//! The venue page: lights, placement and groups share one body.
//!
//! # One selection
//!
//! The stage, the fixture table, the footprint and the groups panel read and
//! write [`Patch::selected`]. Nothing on the page has a mode that changes what
//! a click on a fixture means.
//!
//! # One allocator, and it is not here
//!
//! Every address on this page came from the backend: `next_addresses` for a
//! new one, `set_fixture_address` for a typed one, `auto_patch` for a derived
//! one. The page never picks a free slot, never widens a footprint, never
//! decides a mode's channel count. It shows refusals — that is its half of the
//! contract, and it is why [`Refusal`] is a first-class piece of state rather
//! than a toast.
//!
//! # Why the rows are a plain column
//!
//! Not a `uniform_list`: an editable cell is a live [`TextInput`] entity with a
//! focus handle, and a virtualized list drops the off-screen ones out of the
//! frame — which would take the caret with them mid-edit. A venue's patch is
//! tens to hundreds of rows and this is a page, not a timeline.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use gpui::prelude::*;
use gpui::Focusable as _;
use gpui::{
    div, px, AnyElement, Bounds, Context, Entity, Pixels, Point, SharedString, Subscription, Window,
};

use luma_lib::models::fixtures::PatchedFixture;
use luma_lib::models::patch::{DmxRoute, OutputDevice, OutputPort};
use luma_ui::arg::number::{DraftedNumber, NumberEvent};
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::text_input::TextInput;

use crate::confirm::{Action, Confirm};
use crate::library::Patch as PatchData;
use crate::shell::Body;
use crate::workspace::Target;
use crate::{LibraryError, Luma};

mod footprint;
pub(crate) mod groups;
mod outputs;
mod table;
mod venue;

/// What the page under the stage is showing.
///
/// Two readings of the same fixtures: as an inventory with its groups, and as
/// channels in universes with where each universe comes out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum View {
    Fixtures,
    Patch,
}

impl View {
    pub(crate) const ALL: [View; 2] = [View::Fixtures, View::Patch];

    pub(crate) fn label(self) -> &'static str {
        match self {
            View::Fixtures => "Fixtures",
            View::Patch => "Patch",
        }
    }
}

/// Which cell of a row an edit is in.
///
/// A closed list because it is also the list of things this page may write:
/// three of them are patch facts, and there is deliberately no fourth that is
/// a position.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Column {
    Label,
    Universe,
    Address,
}

/// What a click on a fixture's row does to the selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pick {
    /// Select this row and no other.
    Only,
    /// Add this row, or take it out.
    Toggle,
    /// Select the run from the last row clicked through this one.
    Through,
}

/// The one cell being edited, and the live field in it.
pub(crate) struct Editing {
    pub(crate) fixture: String,
    pub(crate) column: Column,
    pub(crate) label: Option<Entity<TextInput>>,
    pub(crate) number: Option<Entity<DraftedNumber>>,
    /// What the cell read when the caret arrived. A commit that matches it is
    /// not a rename, which is what makes "commit on the way out" free of
    /// writes nobody asked for.
    opened_on: SharedString,
    _subscriptions: Vec<Subscription>,
}

/// A refusal from the allocator, parked against the row that earned it.
///
/// Held rather than shown and forgotten: the stored value did not change, so
/// the row must not move, and the sentence explaining why has to outlive the
/// keystroke that caused it. Cleared by the next edit on that row.
pub(crate) struct Refusal {
    pub(crate) fixture: String,
    pub(crate) message: SharedString,
}

/// A sentence about the write that just happened, and the reload that write
/// issued.
///
/// Scoped to one load because a notice is about an *act*: it survives the
/// reload its own act asked for and nothing after it. The next change to the
/// data is a different story, and a stale line under the title would be
/// claiming to describe it.
pub(crate) struct Notice {
    pub(crate) message: SharedString,
    until: u64,
}

/// A drag along a universe's strip: the fixture picked up, which of its
/// channels was grabbed, and where the pointer is now.
pub(crate) struct StripDrag {
    pub(crate) fixture: String,
    /// Zero-based channel of the fixture the press landed on, so the block
    /// lands where the pointer says rather than one edge of it.
    pub(crate) grabbed_channel: u16,
    /// The universe whose strip the pointer is over, which need not be the
    /// one the block was picked up from.
    pub(crate) universe: u16,
    pub(crate) over: u16,
}

pub(crate) struct Patch {
    /// The venue tab's chat: the venue's own conversations.
    pub(crate) chat: crate::agent::BodyChat,
    pub(crate) venue_id: String,
    pub(crate) venue_name: String,
    pub(crate) stage: crate::stage::StagePage,
    /// The group name being typed, if one is.
    pub(crate) naming: Option<groups::Naming>,
    /// Why the name being typed was refused — one the backend will not take —
    /// under its field until the next commit.
    pub(crate) group_refusal: Option<SharedString>,
    /// A group write is on its way; the panel's controls wait for it.
    pub(crate) group_busy: bool,
    /// A group's menu, and the point it hangs from.
    pub(crate) group_menu: Option<(String, Point<gpui::Pixels>)>,
    pub(crate) data: Option<PatchData>,
    /// The patch could not be read. The cause is in the log.
    pub(crate) load_failed: bool,

    pub(crate) view: View,
    /// Where each universe's strip was last drawn, so a pointer on it can be
    /// read as a channel.
    pub(crate) strips: Rc<RefCell<BTreeMap<u16, Bounds<Pixels>>>>,
    pub(crate) drag: Option<StripDrag>,
    /// Why the last drop was refused, and the universe it was dropped in.
    pub(crate) strip_refusal: Option<(u16, SharedString)>,

    /// The DMX devices the drivers can see, or why there are none.
    pub(crate) devices: Vec<OutputDevice>,
    pub(crate) devices_unavailable: Option<SharedString>,
    /// Which universe row has its device menu open.
    pub(crate) bind_menu: Option<i64>,

    pub(crate) selected: HashSet<String>,
    /// The row a shift-click runs from: the last one clicked on its own.
    pub(crate) anchor: Option<String>,
    pub(crate) editing: Option<Editing>,
    pub(crate) refusal: Option<Refusal>,
    /// The row menu, and the point it hangs from.
    pub(crate) menu: Option<(String, Point<gpui::Pixels>)>,
    /// The mode menu, hanging off a row's mode cell.
    pub(crate) mode_menu: Option<(String, Point<gpui::Pixels>)>,
    /// What the last act did, in one line, and the load it outlives.
    pub(crate) notice: Option<Notice>,
    /// Bumped on every load; a landing older than this one is dropped.
    generation: u64,
}

impl Patch {
    pub(crate) fn loading(
        venue_id: String,
        venue_name: String,
        chat: crate::agent::BodyChat,
        cx: &mut Context<Luma>,
    ) -> Self {
        Self {
            chat,
            venue_id,
            stage: crate::stage::StagePage::new(cx),
            naming: None,
            group_refusal: None,
            group_busy: false,
            group_menu: None,
            venue_name,
            data: None,
            load_failed: false,
            view: View::Fixtures,
            strips: Rc::default(),
            drag: None,
            strip_refusal: None,
            devices: Vec::new(),
            devices_unavailable: None,
            bind_menu: None,
            selected: HashSet::new(),
            anchor: None,
            editing: None,
            refusal: None,
            menu: None,
            mode_menu: None,
            notice: None,
            generation: 0,
        }
    }

    pub(crate) fn rows(&self) -> &[PatchedFixture] {
        self.data.as_ref().map_or(&[], |data| &data.fixtures)
    }

    pub(crate) fn row(&self, id: &str) -> Option<&PatchedFixture> {
        self.rows().iter().find(|row| row.id == id)
    }

    /// The groups a fixture is in, in the panel's order.
    pub(crate) fn groups_of(&self, id: &str) -> Vec<&luma_lib::models::groups::GroupTreeNode> {
        self.data.as_ref().map_or_else(Vec::new, |data| {
            data.groups
                .iter()
                .filter(|group| group.fixtures.iter().any(|fixture| fixture == id))
                .collect()
        })
    }

    /// The modes the fixture's definition offers, with each one's width.
    /// Every mode the row's definition offers: name, width, and whether
    /// Luma can light the fixture in it.
    pub(crate) fn modes(&self, row: &PatchedFixture) -> Vec<(String, usize, bool)> {
        self.data
            .as_ref()
            .and_then(|data| data.definitions.get(&row.fixture_path))
            .map(|def| {
                def.modes
                    .iter()
                    .map(|mode| {
                        let supported = def.supports_mode(&mode.name);
                        (mode.name.clone(), mode.channels.len(), supported)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether Luma can light `row` in its patched mode.
    pub(crate) fn supported(&self, row: &PatchedFixture) -> bool {
        self.data
            .as_ref()
            .and_then(|data| data.definitions.get(&row.fixture_path))
            .is_none_or(|def| def.supports_mode(&row.mode_name))
    }

    pub(crate) fn any_pinned(&self) -> bool {
        self.rows().iter().any(|row| row.address_pinned)
    }

    fn clear_edit(&mut self) {
        self.editing = None;
    }

    /// An edit to a cell is about its row, so a row outside the selection
    /// becomes it. A row already in a larger selection leaves it alone.
    fn select_edited(&mut self, fixture: &str) {
        if !self.selected.contains(fixture) {
            self.selected.clear();
            self.selected.insert(fixture.to_string());
            self.anchor = Some(fixture.to_string());
        }
    }

    /// Park a sentence under the title for the reload this act is about to
    /// issue. `reload_patch` bumps the generation immediately after, which is
    /// the load the notice is allowed to survive.
    fn say(&mut self, message: impl Into<SharedString>) {
        self.notice = Some(Notice {
            message: message.into(),
            until: self.generation + 1,
        });
    }
}

// ---------------------------------------------------------------------------
// Opening and loading
// ---------------------------------------------------------------------------

impl Luma {
    /// Open the venue page in Rig, or bring Rig on screen. `thread` is
    /// the chat a restored body had open.
    pub(crate) fn open_rig(&mut self, thread: Option<&str>, cx: &mut Context<Self>) {
        let Some(browser) = &self.songs else {
            return;
        };
        let venue_id = browser.venue_id().to_string();
        let venue_name = browser.venue_name().to_string();
        let target = Target::Venue {
            venue: venue_id.clone(),
        };
        if self.workspace.body(&target).is_some() {
            self.workspace.set_mode(target.mode());
            cx.notify();
            return;
        }
        let chat = self.body_chat(&target, thread, cx);
        let state = Patch::loading(venue_id.clone(), venue_name, chat, cx);
        self.open_body(target, move || Body::Patch(Box::new(state)), cx);
        self.reload_patch(venue_id, cx);
    }

    /// Re-read the whole page: the patch, the strip and the network.
    ///
    /// Every write ends here rather than patching the row it changed in place.
    /// A local edit is a guess at what the allocator did, and the one case that
    /// matters — a mode change the allocator answered by *moving* the fixture —
    /// is exactly the case a guess gets wrong.
    pub(crate) fn reload_patch(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let target = Target::Venue {
            venue: venue_id.clone(),
        };
        let Some(Body::Patch(state)) = self.workspace.body_mut(&target) else {
            return;
        };
        state.generation += 1;
        let generation = state.generation;
        let data = self.library.patch_data(&venue_id);
        let devices = self.library.output_devices();
        cx.spawn(async move |this, cx| {
            let data = data.await;
            let devices = devices.await;
            this.update(cx, |this, cx| {
                let Some(Body::Patch(state)) = this.workspace.body_mut(&target) else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                state.landed(data, devices);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Patch {
    fn landed(
        &mut self,
        data: Result<PatchData, LibraryError>,
        devices: Result<Vec<OutputDevice>, LibraryError>,
    ) {
        // A load the notice did not ask for: whatever it says happened is not
        // what these rows are.
        if self
            .notice
            .as_ref()
            .is_some_and(|n| n.until < self.generation)
        {
            self.notice = None;
        }
        match data {
            Ok(mut data) => {
                // A rental sheet reads down the patch, not down a table of
                // row ids: `get_patched_fixtures` orders by primary key, which
                // is a uuid and therefore an arbitrary order that changes as
                // fixtures are added. Sorting here is presentation — the stored
                // order is nobody's business.
                data.fixtures
                    .sort_by_key(|row| (row.universe, row.address, row.id.clone()));
                self.selected
                    .retain(|id| data.fixtures.iter().any(|f| &f.id == id));
                if self
                    .anchor
                    .as_ref()
                    .is_some_and(|anchor| !data.fixtures.iter().any(|f| &f.id == anchor))
                {
                    self.anchor = None;
                }
                self.data = Some(data);
                self.load_failed = false;
            }
            Err(error) => {
                log::warn!("could not read the patch of {}: {error}", self.venue_id);
                self.load_failed = true;
            }
        }
        match devices {
            Ok(devices) => {
                self.devices = devices;
                self.devices_unavailable = None;
            }
            // A host with no output at all is a state the panel says out loud;
            // an empty list would read as an empty room. The seam says that
            // in its own sentence, as a not-found; anything else is a bug,
            // logged.
            Err(error) => {
                self.devices.clear();
                self.devices_unavailable = Some(match error.command() {
                    Some(luma_lib::error::CommandError::NotFound(message)) => {
                        message.clone().into()
                    }
                    _ => {
                        log::warn!("could not list DMX devices: {error}");
                        "Could not list the DMX devices.".into()
                    }
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Editing
// ---------------------------------------------------------------------------

impl Luma {
    fn patch_mut(&mut self, venue_id: &str) -> Option<&mut Patch> {
        match self.workspace.body_mut(&Target::Venue {
            venue: venue_id.to_string(),
        }) {
            Some(Body::Patch(state)) => Some(state),
            _ => None,
        }
    }

    /// Put the caret in one cell. Any other edit in flight is dropped — one
    /// live field at a time, because one caret is what a person has.
    pub(crate) fn edit_patch_cell(
        &mut self,
        venue_id: String,
        fixture: String,
        column: Column,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        let Some(row) = state.row(&fixture).cloned() else {
            return;
        };
        // A new edit on the refused row is the answer to the refusal.
        if state.refusal.as_ref().is_some_and(|r| r.fixture == fixture) {
            state.refusal = None;
        }
        state.select_edited(&fixture);
        state.clear_edit();
        let name = row.label.clone().unwrap_or_else(|| row.model.clone());
        let editing = match column {
            Column::Label => {
                let field = cx.new(|cx| {
                    let mut input = TextInput::search("Label", cx);
                    input.set_text(name.clone(), cx);
                    input
                });
                // Typing edits the field and nothing else. The rename is one
                // write at the end, on the same two commit points a
                // `DraftedNumber` uses — blur here, `enter` in the cell — so a
                // five-letter name is one row in the undo history of the patch
                // rather than five.
                let edited = cx.subscribe(&field, move |_: &mut Luma, _, _, cx| cx.notify());
                let venue = venue_id.clone();
                let id = fixture.clone();
                let luma = cx.entity().downgrade();
                let blur =
                    window.on_focus_out(&field.read(cx).focus_handle(cx), cx, move |_, _, cx| {
                        let (venue, id) = (venue.clone(), id.clone());
                        luma.update(cx, |this, cx| this.commit_patch_label(venue, id, cx))
                            .ok();
                    });
                Editing {
                    fixture: fixture.clone(),
                    column,
                    label: Some(field),
                    number: None,
                    opened_on: name.clone().into(),
                    _subscriptions: vec![edited, blur],
                }
            }
            Column::Universe | Column::Address => {
                let (value, max, suffix) = match column {
                    Column::Universe => (row.universe as f64, 32767.0, "universe"),
                    _ => (row.address as f64, 512.0, "address"),
                };
                let field = cx.new(|cx| {
                    DraftedNumber::new(
                        format!("{name} {suffix}"),
                        value,
                        1.0,
                        max,
                        NUMBER_FIELD_WIDTH,
                        window,
                        cx,
                    )
                });
                let venue = venue_id.clone();
                let id = fixture.clone();
                let subscription = cx.subscribe(
                    &field,
                    move |this: &mut Luma, _, event: &NumberEvent, cx| {
                        let NumberEvent::Committed(value) = *event;
                        this.commit_patch_number(&venue, &id, column, value, cx);
                    },
                );
                Editing {
                    fixture: fixture.clone(),
                    column,
                    label: None,
                    number: Some(field),
                    opened_on: value.to_string().into(),
                    _subscriptions: vec![subscription],
                }
            }
        };
        let focus = match (&editing.label, &editing.number) {
            (Some(field), _) => Some(field.read(cx).focus_handle(cx)),
            (_, Some(field)) => Some(field.read(cx).focus_handle(cx)),
            _ => None,
        };
        if let Some(state) = self.patch_mut(&venue_id) {
            state.editing = Some(editing);
        }
        if let Some(focus) = focus {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Write the drafted name down, if it is a different name.
    ///
    /// Idempotent on purpose: blur and `enter` are both commit points and a
    /// person can hit both, so the second one has to be a no-op rather than a
    /// second write of the same string.
    pub(crate) fn commit_patch_label(
        &mut self,
        venue_id: String,
        fixture: String,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        let Some(edit) = state
            .editing
            .as_ref()
            .filter(|edit| edit.fixture == fixture && edit.column == Column::Label)
        else {
            return;
        };
        let Some(field) = edit.label.clone() else {
            return;
        };
        let label = field.read(cx).text().trim().to_string();
        let unchanged = label == edit.opened_on.as_ref();
        state.clear_edit();
        if unchanged || label.is_empty() {
            cx.notify();
            return;
        }
        let pending = self
            .library
            .rename_patched_fixture(&venue_id, &fixture, &label);
        cx.spawn(async move |this, cx| {
            pending.await.ok();
            this.update(cx, |this, cx| this.reload_patch(venue_id, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// A typed universe or address, put to the allocator.
    fn commit_patch_number(
        &mut self,
        venue_id: &str,
        fixture: &str,
        column: Column,
        value: f64,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.patch_mut(venue_id) else {
            return;
        };
        let Some(row) = state.row(fixture).cloned() else {
            return;
        };
        #[allow(clippy::cast_possible_truncation)]
        let typed = value.round() as i64;
        let (universe, address) = match column {
            Column::Universe => (typed, row.address),
            _ => (row.universe, typed),
        };
        let venue = venue_id.to_string();
        let id = fixture.to_string();
        let pending = self
            .library
            .set_fixture_address(venue_id, fixture, universe, address);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    // Committed: the caret has nothing left to say. The label
                    // field stays open because a name is still being typed;
                    // a number is done the moment it lands.
                    if let Some(state) = this.patch_mut(&venue) {
                        state.clear_edit();
                    }
                    this.reload_patch(venue, cx);
                }
                Err(error) => {
                    // Refused. The row keeps the address it had — nothing was
                    // written — and the sentence saying why sits under it until
                    // the next edit on that row.
                    let Some(message) = refusal(&error) else {
                        if let Some(state) = this.patch_mut(&venue) {
                            state.clear_edit();
                        }
                        return this.report_failure("The address was not saved", error, cx);
                    };
                    if let Some(state) = this.patch_mut(&venue) {
                        state.clear_edit();
                        state.refusal = Some(Refusal {
                            fixture: id,
                            message,
                        });
                    }
                    this.select_occupant(venue, universe, address, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Put the selection on whatever already holds `universe`:`address`.
    ///
    /// The refusal names the fixture in the way; this is the page pointing at
    /// it. Who that is comes from `universe_occupancy` — the one thing that
    /// answers "what is at this address" — rather than from a scan of the rows
    /// here, which would be the second occupancy rule this page exists not to
    /// have. An address nothing holds simply selects nothing, so an
    /// out-of-range refusal needs no branch of its own.
    fn select_occupant(
        &mut self,
        venue_id: String,
        universe: i64,
        address: i64,
        cx: &mut Context<Self>,
    ) {
        let Ok(universe) = u16::try_from(universe) else {
            return;
        };
        let Ok(address) = u16::try_from(address) else {
            return;
        };
        let pending = self.library.universe_occupancy(&venue_id, universe);
        cx.spawn(async move |this, cx| {
            let cells = pending.await.unwrap_or_default();
            let occupant = cells
                .iter()
                .find(|cell| cell.address == address)
                .and_then(|cell| cell.fixture_id.clone());
            this.update(cx, |this, cx| {
                if let (Some(occupant), Some(state)) = (occupant, this.patch_mut(&venue_id)) {
                    state.selected.clear();
                    state.selected.insert(occupant);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Repatch a row into another mode, asking first if it would have to move.
    pub(crate) fn set_patch_mode(
        &mut self,
        venue_id: String,
        fixture: String,
        mode_name: String,
        allow_move: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.mode_menu = None;
        }
        let name = self
            .patch_mut(&venue_id)
            .and_then(|state| state.row(&fixture).cloned())
            .and_then(|row| row.label)
            .unwrap_or_else(|| fixture.clone());
        let pending = self
            .library
            .set_fixture_mode(&venue_id, &fixture, &mode_name, allow_move);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| match result {
                Ok(_) => this.reload_patch(venue_id, cx),
                Err(error) if !allow_move && refusal(&error).is_some() => {
                    // The refusal *is* the question: the width does not fit
                    // where it stands, so ask before letting the allocator
                    // move it. Only an addressing refusal asks it — offering
                    // to move a fixture because the database was unreachable
                    // would be a question with no answer.
                    this.ask(
                        Confirm {
                            title: format!("Move {name} to fit {mode_name}?").into(),
                            body: refusal(&error).unwrap_or_default(),
                            verb: "Repatch".into(),
                            action: Action::RepatchMode {
                                venue_id: venue_id.into(),
                                fixture_id: fixture.into(),
                                mode_name: mode_name.into(),
                            },
                        },
                        cx,
                    );
                }
                Err(error) => match refusal(&error) {
                    Some(message) => {
                        if let Some(state) = this.patch_mut(&venue_id) {
                            state.refusal = Some(Refusal { fixture, message });
                        }
                        cx.notify();
                    }
                    None => this.report_failure("The mode was not changed", error, cx),
                },
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn set_patch_pin(
        &mut self,
        venue_id: String,
        fixture: String,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.menu = None;
        }
        let pending = self.library.set_address_pinned(&venue_id, &fixture, pinned);
        cx.spawn(async move |this, cx| {
            pending.await.ok();
            this.update(cx, |this, cx| this.reload_patch(venue_id, cx))
                .ok();
        })
        .detach();
    }

    /// Ask before unpatching: every light stands in the room.
    pub(crate) fn unpatch_selection(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        state.menu = None;
        let ids: Vec<String> = state
            .rows()
            .iter()
            .filter(|row| state.selected.contains(&row.id))
            .map(|row| row.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        let count = ids.len();
        self.ask(
            Confirm {
                title: if count == 1 {
                    "Unpatch this fixture?".into()
                } else {
                    format!("Unpatch {count} fixtures?").into()
                },
                body: "Unpatching removes it from the room as well as from the patch.".into(),
                verb: "Unpatch".into(),
                action: Action::UnpatchFixtures {
                    venue_id: venue_id.into(),
                    fixture_ids: ids,
                },
            },
            cx,
        );
    }

    pub(crate) fn run_unpatch(
        &mut self,
        venue_id: String,
        ids: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let pending: Vec<_> = ids
            .iter()
            .map(|id| self.library.remove_patched_fixture(&venue_id, id))
            .collect();
        cx.spawn(async move |this, cx| {
            let mut failure = None;
            for call in pending {
                if let Err(error) = call.await {
                    // The first failure is the one worth logging; the rest are
                    // usually the same one about the same venue.
                    failure.get_or_insert(error);
                }
            }
            this.update(cx, |this, cx| {
                if let Some(error) = failure {
                    this.report_failure("Some fixtures were not removed", error, cx);
                }
                this.reload_patch(venue_id, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Auto patch. A venue holding pinned addresses is asked first, because
    /// the answer discards them.
    pub(crate) fn auto_patch_venue(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let pinned = self
            .patch_mut(&venue_id)
            .is_some_and(|state| state.any_pinned());
        if !pinned {
            self.run_auto_patch(venue_id, cx);
            return;
        }
        self.ask(
            Confirm {
                title: "Re-derive every address?".into(),
                body: "This venue holds hand-set addresses. Auto patch derives \
                       addresses from where fixtures hang and discards the \
                       overrides it touches."
                    .into(),
                verb: "Auto patch".into(),
                action: Action::AutoPatch {
                    venue_id: venue_id.into(),
                },
            },
            cx,
        );
    }

    pub(crate) fn run_auto_patch(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let pending = self.library.auto_patch(&venue_id);
        cx.spawn(async move |this, cx| {
            let report = pending.await;
            this.update(cx, |this, cx| {
                let notice: SharedString = match report {
                    Ok(report) => {
                        let mut line = format!(
                            "Auto patch moved {}, discarded {} overrides",
                            report.moved, report.overrides_discarded
                        );
                        for note in &report.notes {
                            line.push_str(" · ");
                            line.push_str(&note.message);
                        }
                        line.into()
                    }
                    Err(error) => {
                        this.report_failure("Auto patch did not run", error, cx);
                        return this.reload_patch(venue_id, cx);
                    }
                };
                if let Some(state) = this.patch_mut(&venue_id) {
                    state.say(notice);
                }
                this.reload_patch(venue_id, cx);
            })
            .ok();
        })
        .detach();
    }

    // -- selection and menus --------------------------------------------------

    pub(crate) fn pick_patch_row(
        &mut self,
        venue_id: String,
        fixture: String,
        pick: Pick,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.clear_edit();
            match pick {
                Pick::Only => {
                    state.selected.clear();
                    state.selected.insert(fixture.clone());
                    state.anchor = Some(fixture);
                }
                Pick::Toggle => {
                    if !state.selected.remove(&fixture) {
                        state.selected.insert(fixture.clone());
                    }
                    state.anchor = Some(fixture);
                }
                // The run is read down the table as it is drawn, from the
                // anchor, which stays where it was so a second shift-click
                // re-aims the same run.
                Pick::Through => {
                    let rows = state.rows();
                    let end = rows.iter().position(|row| row.id == fixture);
                    let start = state
                        .anchor
                        .as_ref()
                        .and_then(|anchor| rows.iter().position(|row| &row.id == anchor))
                        .or(end);
                    if let (Some(start), Some(end)) = (start, end) {
                        let run: HashSet<String> = rows[start.min(end)..=start.max(end)]
                            .iter()
                            .map(|row| row.id.clone())
                            .collect();
                        state.selected = run;
                        if state.anchor.is_none() {
                            state.anchor = Some(fixture);
                        }
                    }
                }
            }
        }
        let selected = self
            .patch_mut(&venue_id)
            .map(|state| state.selected.clone())
            .unwrap_or_default();
        self.highlight_venue_lights(selected, cx);
        cx.notify();
    }

    pub(crate) fn open_patch_menu(
        &mut self,
        venue_id: String,
        fixture: String,
        at: Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            // Right-clicking outside the selection replaces it: a menu that
            // acted on rows the pointer was nowhere near is how people lose
            // fixtures.
            if !state.selected.contains(&fixture) {
                state.selected.clear();
                state.selected.insert(fixture.clone());
            }
            state.menu = Some((fixture, at));
        }
        cx.notify();
    }

    pub(crate) fn open_patch_mode_menu(
        &mut self,
        venue_id: String,
        fixture: String,
        at: Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.select_edited(&fixture);
            state.mode_menu = Some((fixture, at));
        }
        let selected = self
            .patch_mut(&venue_id)
            .map(|state| state.selected.clone())
            .unwrap_or_default();
        self.highlight_venue_lights(selected, cx);
    }

    pub(crate) fn close_patch_menus(&mut self, venue_id: String, cx: &mut Context<Self>) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.menu = None;
            state.mode_menu = None;
            state.bind_menu = None;
            state.group_menu = None;
        }
        cx.notify();
    }

    // -- the universe strips ---------------------------------------------------

    /// Show the fixtures as an inventory, or as channels in universes.
    pub(crate) fn show_patch_view(&mut self, venue_id: String, view: View, cx: &mut Context<Self>) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.view = view;
            state.clear_edit();
            state.drag = None;
            state.menu = None;
            state.mode_menu = None;
            state.bind_menu = None;
        }
        cx.notify();
    }

    /// Pick a fixture's block up by the channel under the pointer.
    pub(crate) fn grab_strip_block(
        &mut self,
        venue_id: String,
        fixture: String,
        universe: u16,
        address: u16,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        state.strip_refusal = None;
        let Some(row) = state.row(&fixture) else {
            return;
        };
        let grabbed_channel = u16::try_from(i64::from(address) - row.address)
            .unwrap_or(0)
            .min(u16::try_from(row.num_channels - 1).unwrap_or(0));
        state.selected.clear();
        state.selected.insert(fixture.clone());
        state.anchor = Some(fixture.clone());
        state.drag = Some(StripDrag {
            fixture,
            grabbed_channel,
            universe,
            over: address,
        });
        let selected = state.selected.clone();
        self.highlight_venue_lights(selected, cx);
    }

    pub(crate) fn drag_strip_to(
        &mut self,
        venue_id: String,
        universe: u16,
        address: u16,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            let Some(drag) = state.drag.as_mut() else {
                return;
            };
            if drag.over == address && drag.universe == universe {
                return;
            }
            drag.over = address;
            drag.universe = universe;
        }
        cx.notify();
    }

    /// Drop the dragged block. The new start is where the *grabbed channel*
    /// lands, so a block picked up by its middle does not jump.
    pub(crate) fn drop_strip_block(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        let Some(drag) = state.drag.take() else {
            return;
        };
        let strip = drag.universe;
        let universe = i64::from(strip);
        let start = i64::from(drag.over) - i64::from(drag.grabbed_channel);
        let fixture = drag.fixture;
        if state
            .row(&fixture)
            .is_some_and(|row| row.universe == universe && row.address == start)
        {
            cx.notify();
            return;
        }
        let pending = self
            .library
            .set_fixture_address(&venue_id, &fixture, universe, start);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| match result {
                Ok(()) => this.reload_patch(venue_id, cx),
                Err(error) => {
                    let Some(message) = refusal(&error) else {
                        return this.report_failure("The fixture was not moved", error, cx);
                    };
                    if let Some(state) = this.patch_mut(&venue_id) {
                        state.strip_refusal = Some((strip, message));
                    }
                    this.select_occupant(venue_id, universe, start, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // -- outputs ---------------------------------------------------------------

    pub(crate) fn open_bind_menu(
        &mut self,
        venue_id: String,
        universe: i64,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.bind_menu = (state.bind_menu != Some(universe)).then_some(universe);
        }
        cx.notify();
    }

    pub(crate) fn bind_universe(
        &mut self,
        venue_id: String,
        universe: i64,
        device: OutputDevice,
        port: OutputPort,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.bind_menu = None;
        }
        let pending = self.library.bind_route(universe, &device, &port);
        cx.spawn(async move |this, cx| {
            pending.await.ok();
            this.update(cx, |this, cx| this.reload_patch(venue_id, cx))
                .ok();
        })
        .detach();
    }

    pub(crate) fn unbind_universe(
        &mut self,
        venue_id: String,
        route: DmxRoute,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.bind_menu = None;
        }
        let pending = self.library.unbind_route(&route);
        cx.spawn(async move |this, cx| {
            pending.await.ok();
            this.update(cx, |this, cx| this.reload_patch(venue_id, cx))
                .ok();
        })
        .detach();
    }
}

/// The backend's sentence for a refusal a person can act on where they are —
/// an address taken, a footprint off the end of a universe, a group name it
/// will not take — or `None` for a failure that is not theirs to fix.
///
/// The seam draws that line already: refusals arrive as
/// [`CommandError::Invalid`], whose sentence names the universe, the address
/// and the fixture in the way, and everything else as an internal failure,
/// which is reported in the status bar and logged. The sentence is the
/// command's, without the wire verb the seam prefixes.
pub(crate) fn refusal(error: &LibraryError) -> Option<SharedString> {
    match error.command() {
        Some(invalid @ luma_lib::error::CommandError::Invalid(_)) => {
            Some(invalid.to_string().into())
        }
        _ => None,
    }
}

/// Wide enough for `512` and its caret with room to spare, narrow enough that
/// two of them plus a range still fit the row.
pub(crate) const NUMBER_FIELD_WIDTH: f32 = 56.0;

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

/// The venue page beneath the stage: the fixture table and the groups panel.
pub(crate) fn patch(state: &Patch, app: &Entity<Luma>) -> AnyElement {
    venue::render(state, app)
}

/// What the table holds, what was just done, and the page's own acts.
fn band(state: &Patch, app: &Entity<Luma>) -> impl IntoElement {
    let auto = app.clone();
    let for_auto = state.venue_id.clone();
    div()
        .flex_shrink_0()
        .h(px(BAND_HEIGHT))
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(luma_ui::float::segmented().children(View::ALL.map(|view| {
            let shown = app.clone();
            let venue = state.venue_id.clone();
            luma_ui::float::segment(view.label(), state.view == view, view.label())
                .id(SharedString::from(format!("patch-view-{}", view.label())))
                .on_click(move |_, _, cx| {
                    let venue = venue.clone();
                    shown.update(cx, |this, cx| this.show_patch_view(venue, view, cx));
                })
                .agent_node(Role::Toggle, view.label())
                .agent_focused(state.view == view)
        })))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .text_color(ladder::foreground_alpha(0.55))
                .child(subtitle(state))
                .agent_node(Role::Text, subtitle(state)),
        )
        .child(
            luma_ui::float::btn("Auto patch", "patch-auto")
                .id("patch-auto")
                .on_click(move |_, _, cx| {
                    let venue = for_auto.clone();
                    auto.update(cx, |this, cx| this.auto_patch_venue(venue, cx));
                })
                .agent_node(Role::Button, "Auto patch"),
        )
}

/// The line under the title: what the last act did, or what the patch holds.
fn subtitle(state: &Patch) -> SharedString {
    if let Some(notice) = &state.notice {
        return notice.message.clone();
    }
    if state.load_failed {
        return "Could not read the patch".into();
    }
    let Some(data) = state.data.as_ref() else {
        return "Reading the patch…".into();
    };
    let mut line = format!(
        "{} · {}",
        plural(data.fixtures.len(), "fixture"),
        plural(data.universes.len(), "universe"),
    );
    if !state.selected.is_empty() {
        line.push_str(&format!(" · {} selected", state.selected.len()));
    }
    line.into()
}

/// `n thing`, `n things`. One helper rather than an `s` at each call site,
/// because "1 universes" is the kind of thing nobody notices until it ships.
pub(crate) fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// The row of chrome over the table, and over the groups beside it, so the
/// two lists start on one line.
pub(crate) const BAND_HEIGHT: f32 = 44.0;
