//! The patch page: what a rig *is*, as paperwork.
//!
//! The venue graph's two pages split on one question each — the stage page
//! answers *where*, this one answers *what* — and the split is enforced rather
//! than merely intended: nothing here writes a `venue_edges` row or a
//! placement param, and there is no column, field or gesture that could
//! (gauntlet AF8). A fixture's placement reaches this page as one bit,
//! [`crate::library::Patch::placed`], because "is it in the tray" is the only
//! thing a rental sheet needs to know about the room.
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

use std::collections::{BTreeMap, HashSet};

use gpui::prelude::*;
use gpui::Focusable as _;
use gpui::{div, px, AnyElement, Context, Entity, Point, SharedString, Subscription, Window};

use luma_lib::models::fixtures::PatchedFixture;
use luma_lib::models::patch::{ArtNetNode, UniverseCell};
use luma_ui::arg::number::{DraftedNumber, NumberEvent};
use luma_ui::ladder;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};
use luma_ui::text_input::{self, TextInput};

use crate::confirm::{Action, Confirm};
use crate::library::Patch as PatchData;
use crate::shell::Body;
use crate::tabs::Target;
use crate::{LibraryError, Luma};

mod add;
mod footprint;
mod groups;
mod outputs;
mod table;

pub(crate) use add::render as add_fixtures_dialog;
pub(crate) use add::tick as tick_add_fixtures;
pub(crate) use add::AddFixtures;

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

/// The one cell being edited, and the live field in it.
pub(crate) struct Editing {
    pub(crate) fixture: String,
    pub(crate) column: Column,
    pub(crate) label: Option<Entity<TextInput>>,
    pub(crate) number: Option<Entity<DraftedNumber>>,
    _subscription: Subscription,
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

/// A drag across the footprint strip: the fixture picked up, which of its
/// channels was grabbed, and where the pointer is now.
pub(crate) struct StripDrag {
    pub(crate) fixture: String,
    /// Zero-based channel of the fixture the press landed on, so the block
    /// lands where the pointer says rather than one edge of it.
    pub(crate) grabbed_channel: u16,
    pub(crate) over: u16,
}

pub(crate) struct Patch {
    pub(crate) venue_id: String,
    pub(crate) venue_name: String,
    pub(crate) data: Option<PatchData>,
    pub(crate) error: Option<String>,

    /// The universe the strip is showing, and its 512 cells.
    pub(crate) strip_universe: u16,
    pub(crate) cells: Vec<UniverseCell>,
    pub(crate) drag: Option<StripDrag>,
    /// The fixture a strip cell was clicked on, which is also a table selection.
    pub(crate) strip_refusal: Option<SharedString>,

    /// Discovered Art-Net nodes, or why there are none.
    pub(crate) nodes: Vec<ArtNetNode>,
    pub(crate) discovery_error: Option<SharedString>,
    /// Which universe row has its node menu open.
    pub(crate) bind_menu: Option<i64>,

    pub(crate) selected: HashSet<String>,
    pub(crate) editing: Option<Editing>,
    pub(crate) refusal: Option<Refusal>,
    /// The row menu, and the point it hangs from.
    pub(crate) menu: Option<(String, Point<gpui::Pixels>)>,
    /// The mode menu, hanging off a row's mode cell.
    pub(crate) mode_menu: Option<(String, Point<gpui::Pixels>)>,
    /// What the last Auto Patch did, in one line.
    pub(crate) notice: Option<SharedString>,
    /// Bumped on every load; a landing older than this one is dropped.
    generation: u64,
}

impl Patch {
    pub(crate) fn loading(venue_id: String, venue_name: String) -> Self {
        Self {
            venue_id,
            venue_name,
            data: None,
            error: None,
            strip_universe: 1,
            cells: Vec::new(),
            drag: None,
            strip_refusal: None,
            nodes: Vec::new(),
            discovery_error: None,
            bind_menu: None,
            selected: HashSet::new(),
            editing: None,
            refusal: None,
            menu: None,
            mode_menu: None,
            notice: None,
            generation: 0,
        }
    }

    pub(crate) fn venue_name(&self) -> &str {
        &self.venue_name
    }

    pub(crate) fn rows(&self) -> &[PatchedFixture] {
        self.data.as_ref().map_or(&[], |data| &data.fixtures)
    }

    pub(crate) fn row(&self, id: &str) -> Option<&PatchedFixture> {
        self.rows().iter().find(|row| row.id == id)
    }

    pub(crate) fn is_placed(&self, id: &str) -> bool {
        self.data
            .as_ref()
            .is_some_and(|data| data.placed.contains(id))
    }

    /// The group path a fixture sits on, deepest node first joined by ` / `.
    ///
    /// Read out of `list_group_tree` rather than derived: the derivation is the
    /// backend's, and a second walk of the same facts here would be the second
    /// grouping rule the gauntlet forbids.
    pub(crate) fn group_path(&self, id: &str) -> Option<String> {
        let data = self.data.as_ref()?;
        let by_id: BTreeMap<&str, &luma_lib::models::groups::GroupTreeNode> =
            data.groups.iter().map(|n| (n.id.as_str(), n)).collect();
        // The deepest node holding it — the tree is parents-first, so the last
        // match down the list is the leaf.
        let leaf = data
            .groups
            .iter()
            .filter(|node| node.fixtures.iter().any(|f| f == id))
            .next_back()?;
        let mut path = vec![leaf.label.clone()];
        let mut parent = leaf.parent_id.clone();
        while let Some(id) = parent {
            let Some(node) = by_id.get(id.as_str()) else {
                break;
            };
            path.push(node.label.clone());
            parent = node.parent_id.clone();
        }
        path.reverse();
        Some(path.join(" / "))
    }

    /// The modes the fixture's definition offers, with each one's width.
    pub(crate) fn modes(&self, row: &PatchedFixture) -> Vec<(String, usize)> {
        self.data
            .as_ref()
            .and_then(|data| data.definitions.get(&row.fixture_path))
            .map(|def| {
                def.modes
                    .iter()
                    .map(|mode| (mode.name.clone(), mode.channels.len()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether any selected fixture is placed — the question the unpatch
    /// confirmation is asking.
    pub(crate) fn any_selected_placed(&self) -> bool {
        self.selected.iter().any(|id| self.is_placed(id))
    }

    pub(crate) fn any_pinned(&self) -> bool {
        self.rows().iter().any(|row| row.address_pinned)
    }

    fn clear_edit(&mut self) {
        self.editing = None;
    }
}

// ---------------------------------------------------------------------------
// Opening and loading
// ---------------------------------------------------------------------------

impl Luma {
    /// Reveal the selected venue's patch as one target-keyed workspace tab.
    pub(crate) fn open_patch(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let venue_id = browser.venue_id().to_string();
        let venue_name = browser.venue_name().to_string();
        let target = Target::Patch {
            venue: venue_id.clone(),
        };
        if self.workspace.body_mut(&target).is_some() {
            self.workspace.select(&target);
            cx.notify();
            return;
        }
        let state = Patch::loading(venue_id.clone(), venue_name);
        self.open_tab(target, move || Body::Patch(Box::new(state)), cx);
        self.reload_patch(venue_id, cx);
    }

    /// Re-read the whole page: the patch, the strip and the network.
    ///
    /// Every write ends here rather than patching the row it changed in place.
    /// A local edit is a guess at what the allocator did, and the one case that
    /// matters — a mode change the allocator answered by *moving* the fixture —
    /// is exactly the case a guess gets wrong.
    pub(crate) fn reload_patch(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let target = Target::Patch {
            venue: venue_id.clone(),
        };
        let Some(Body::Patch(state)) = self.workspace.body_mut(&target) else {
            return;
        };
        state.generation += 1;
        let generation = state.generation;
        let universe = state.strip_universe;
        let data = self.library.patch_data(&venue_id);
        let cells = self.library.universe_occupancy(&venue_id, universe);
        let nodes = self.library.artnet_nodes();
        cx.spawn(async move |this, cx| {
            let data = data.await;
            let cells = cells.await;
            let nodes = nodes.await;
            this.update(cx, |this, cx| {
                let Some(Body::Patch(state)) = this.workspace.body_mut(&target) else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                state.landed(data, cells, nodes);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-read one universe's 512 cells, without disturbing the table.
    pub(crate) fn show_universe(
        &mut self,
        venue_id: String,
        universe: u16,
        cx: &mut Context<Self>,
    ) {
        let target = Target::Patch {
            venue: venue_id.clone(),
        };
        let Some(Body::Patch(state)) = self.workspace.body_mut(&target) else {
            return;
        };
        state.strip_universe = universe;
        state.strip_refusal = None;
        state.generation += 1;
        let generation = state.generation;
        let pending = self.library.universe_occupancy(&venue_id, universe);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let cells = pending.await;
            this.update(cx, |this, cx| {
                let Some(Body::Patch(state)) = this.workspace.body_mut(&target) else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                state.cells = cells.unwrap_or_default();
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
        cells: Result<Vec<UniverseCell>, LibraryError>,
        nodes: Result<Vec<ArtNetNode>, LibraryError>,
    ) {
        match data {
            Ok(mut data) => {
                // A rental sheet reads down the patch, not down a table of
                // row ids: `get_patched_fixtures` orders by primary key, which
                // is a uuid and therefore an arbitrary order that changes as
                // fixtures are added. Sorting here is presentation — the stored
                // order is nobody's business.
                data.fixtures
                    .sort_by_key(|row| (row.universe, row.address, row.id.clone()));
                // A universe that emptied out from under the strip: fall back
                // to the first one in use rather than drawing 512 blanks and
                // calling it a universe.
                if !data.universes.is_empty() && !data.universes.contains(&self.strip_universe) {
                    self.strip_universe = data.universes[0];
                }
                self.selected
                    .retain(|id| data.fixtures.iter().any(|f| &f.id == id));
                self.data = Some(data);
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        self.cells = cells.unwrap_or_default();
        match nodes {
            Ok(nodes) => {
                self.nodes = nodes;
                self.discovery_error = None;
            }
            // A host with no Art-Net at all is a state the panel says out loud;
            // an empty list would read as a quiet network.
            Err(error) => {
                self.nodes.clear();
                self.discovery_error = Some(error.to_string().into());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Editing
// ---------------------------------------------------------------------------

impl Luma {
    fn patch_mut(&mut self, venue_id: &str) -> Option<&mut Patch> {
        match self.workspace.body_mut(&Target::Patch {
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
        state.clear_edit();
        let name = row.label.clone().unwrap_or_else(|| row.model.clone());
        let editing = match column {
            Column::Label => {
                let field = cx.new(|cx| {
                    let mut input = TextInput::search("Label", cx);
                    input.set_text(name.clone(), cx);
                    input
                });
                let venue = venue_id.clone();
                let id = fixture.clone();
                let subscription =
                    cx.subscribe(&field, move |this: &mut Luma, field, event, cx| {
                        if event == &text_input::Event::Edited {
                            let text = field.read(cx).text().to_string();
                            this.stage_patch_label(&venue, &id, text, cx);
                        } else {
                            cx.notify();
                        }
                    });
                Editing {
                    fixture: fixture.clone(),
                    column,
                    label: Some(field),
                    number: None,
                    _subscription: subscription,
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
                    _subscription: subscription,
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

    /// The label field commits on every keystroke — a name has no allocator to
    /// refuse it, so there is nothing to wait for and nothing to undo.
    fn stage_patch_label(
        &mut self,
        venue_id: &str,
        fixture: &str,
        label: String,
        cx: &mut Context<Self>,
    ) {
        let pending = self
            .library
            .rename_patched_fixture(venue_id, fixture, &label);
        if let Some(state) = self.patch_mut(venue_id) {
            if let Some(row) = state
                .data
                .as_mut()
                .and_then(|d| d.fixtures.iter_mut().find(|r| r.id == fixture))
            {
                row.label = Some(label);
            }
        }
        cx.spawn(async move |_, _| {
            pending.await.ok();
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
                    if let Some(state) = this.patch_mut(&venue) {
                        state.clear_edit();
                        state.refusal = Some(Refusal {
                            fixture: id,
                            message: refusal_message(&error).into(),
                        });
                    }
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
                Err(error) if !allow_move => {
                    // The refusal *is* the question: the width does not fit
                    // where it stands, so ask before letting the allocator
                    // move it.
                    this.ask(
                        Confirm {
                            title: format!("Move {name} to fit {mode_name}?").into(),
                            body: refusal_message(&error).into(),
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
                Err(error) => {
                    if let Some(state) = this.patch_mut(&venue_id) {
                        state.refusal = Some(Refusal {
                            fixture,
                            message: refusal_message(&error).into(),
                        });
                    }
                    cx.notify();
                }
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

    /// One more of each selected fixture, at the allocator's next free slots.
    pub(crate) fn duplicate_patch_rows(&mut self, venue_id: String, cx: &mut Context<Self>) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        state.menu = None;
        let picked: Vec<PatchedFixture> = state
            .rows()
            .iter()
            .filter(|row| state.selected.contains(&row.id))
            .cloned()
            .collect();
        if picked.is_empty() {
            return;
        }
        let batches: Vec<_> = picked
            .into_iter()
            .map(|row| {
                self.library.add_fixtures(
                    &venue_id,
                    crate::library::NewFixtures {
                        manufacturer: row.manufacturer,
                        model: row.model,
                        mode_name: row.mode_name,
                        fixture_path: row.fixture_path,
                        channels: row.num_channels,
                        count: 1,
                    },
                )
            })
            .collect();
        cx.spawn(async move |this, cx| {
            for batch in batches {
                batch.await.ok();
            }
            this.update(cx, |this, cx| this.reload_patch(venue_id, cx))
                .ok();
        })
        .detach();
    }

    /// Ask before unpatching anything that is standing in the room.
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
        let placed = state.any_selected_placed();
        let count = ids.len();
        if !placed {
            self.run_unpatch(venue_id, ids, cx);
            return;
        }
        self.ask(
            Confirm {
                title: if count == 1 {
                    "Unpatch this fixture?".into()
                } else {
                    format!("Unpatch {count} fixtures?").into()
                },
                body: "It is placed in the room. Unpatching removes it from the \
                       structure it hangs on as well as from the patch."
                    .into(),
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
                    // The first refusal is the one worth reading; the rest are
                    // usually the same sentence about the same venue.
                    failure.get_or_insert_with(|| refusal_message(&error));
                }
            }
            this.update(cx, |this, cx| {
                if let (Some(failure), Some(state)) = (failure, this.patch_mut(&venue_id)) {
                    state.notice = Some(failure.into());
                }
                this.reload_patch(venue_id, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Auto Patch. A venue holding pinned addresses is asked first, because
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
                body: "This venue holds hand-set addresses. Auto Patch derives \
                       addresses from where fixtures hang and discards the \
                       overrides it touches."
                    .into(),
                verb: "Auto Patch".into(),
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
                let notice: SharedString = match &report {
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
                    Err(error) => error.to_string().into(),
                };
                if let Some(state) = this.patch_mut(&venue_id) {
                    state.notice = Some(notice);
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
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.clear_edit();
            if extend {
                if !state.selected.remove(&fixture) {
                    state.selected.insert(fixture);
                }
            } else {
                state.selected.clear();
                state.selected.insert(fixture);
            }
        }
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
            state.mode_menu = Some((fixture, at));
        }
        cx.notify();
    }

    pub(crate) fn close_patch_menus(&mut self, venue_id: String, cx: &mut Context<Self>) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.menu = None;
            state.mode_menu = None;
            state.bind_menu = None;
        }
        cx.notify();
    }

    // -- the footprint strip ---------------------------------------------------

    pub(crate) fn grab_strip_cell(
        &mut self,
        venue_id: String,
        address: u16,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.patch_mut(&venue_id) else {
            return;
        };
        state.strip_refusal = None;
        let Some(cell) = state
            .cells
            .iter()
            .find(|cell| cell.address == address)
            .cloned()
        else {
            return;
        };
        let Some(fixture) = cell.fixture_id.clone() else {
            state.drag = None;
            cx.notify();
            return;
        };
        state.selected.clear();
        state.selected.insert(fixture.clone());
        state.drag = Some(StripDrag {
            fixture,
            grabbed_channel: cell.channel,
            over: address,
        });
        cx.notify();
    }

    pub(crate) fn drag_strip_to(&mut self, venue_id: String, address: u16, cx: &mut Context<Self>) {
        if let Some(state) = self.patch_mut(&venue_id) {
            if let Some(drag) = state.drag.as_mut() {
                if drag.over == address {
                    return;
                }
                drag.over = address;
            } else {
                return;
            }
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
        let universe = i64::from(state.strip_universe);
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
                    if let Some(state) = this.patch_mut(&venue_id) {
                        state.strip_refusal = Some(refusal_message(&error).into());
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
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
        node: ArtNetNode,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.patch_mut(&venue_id) {
            state.bind_menu = None;
        }
        let pending = self.library.bind_output(universe, &node);
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
        universe: i64,
        cx: &mut Context<Self>,
    ) {
        let pending = self.library.unbind_output(universe);
        cx.spawn(async move |this, cx| {
            pending.await.ok();
            this.update(cx, |this, cx| this.reload_patch(venue_id, cx))
                .ok();
        })
        .detach();
    }
}

/// What a refused write says, with the command name the seam prefixes stripped.
///
/// The allocator's sentence already names the universe, the address and the
/// fixture in the way; a page that prefixed it with the wire verb would be
/// showing an operator a command name they never typed.
pub(crate) fn refusal_message(error: &LibraryError) -> String {
    error
        .command()
        .map_or_else(|| error.to_string(), std::string::ToString::to_string)
}

/// Wide enough for `512` and its caret with room to spare, narrow enough that
/// two of them plus a range still fit the row.
pub(crate) const NUMBER_FIELD_WIDTH: f32 = 56.0;

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

/// Header, table, and the rail that answers "and where does it come out".
pub(crate) fn patch(state: &Patch, app: &Entity<Luma>, window: &Window) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .child(header(state, app))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(table::table(state, app, window))
                .child(rail(state, app)),
        )
        .children(
            state
                .menu
                .as_ref()
                .map(|(fixture, at)| table::row_menu(state, fixture, *at, app)),
        )
        .children(
            state
                .mode_menu
                .as_ref()
                .map(|(fixture, at)| table::mode_menu(state, fixture, *at, app)),
        )
        .agent_node(Role::Card, format!("{} Patch", state.venue_name))
}

fn header(state: &Patch, app: &Entity<Luma>) -> impl IntoElement {
    let add = app.clone();
    let auto = app.clone();
    let for_add = state.venue_id.clone();
    let for_auto = state.venue_id.clone();
    div()
        .flex_shrink_0()
        .px(px(18.0))
        .py(px(14.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .border_b_1()
        .border_color(ladder::trim())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(luma_ui::silkscreen("PATCH"))
                .child(
                    div()
                        .mt(px(5.0))
                        .text_size(px(12.0))
                        .text_color(ladder::muted_foreground())
                        .child(subtitle(state))
                        .agent_node(Role::Text, subtitle(state)),
                ),
        )
        .child(
            luma_ui::luma_button("Add fixtures", luma_ui::Enabled::Yes)
                .id("patch-add")
                .on_click(move |_, window, cx| {
                    let venue = for_add.clone();
                    add.update(cx, |this, cx| this.open_add_fixtures(venue, window, cx));
                })
                .agent_node(Role::Button, "Add fixtures"),
        )
        .child(
            luma_ui::luma_button("Auto Patch", luma_ui::Enabled::Yes)
                .id("patch-auto")
                .on_click(move |_, _, cx| {
                    let venue = for_auto.clone();
                    auto.update(cx, |this, cx| this.auto_patch_venue(venue, cx));
                })
                .agent_node(Role::Button, "Auto Patch"),
        )
}

/// The line under the title: what the last act did, or what the patch holds.
fn subtitle(state: &Patch) -> SharedString {
    if let Some(notice) = &state.notice {
        return notice.clone();
    }
    if let Some(error) = &state.error {
        return format!("Failed to load the patch: {error}").into();
    }
    let Some(data) = state.data.as_ref() else {
        return "Loading the patch…".into();
    };
    let unplaced = data
        .fixtures
        .iter()
        .filter(|f| !data.placed.contains(&f.id))
        .count();
    format!(
        "{} · {} · {unplaced} in the tray",
        plural(data.fixtures.len(), "fixture"),
        plural(data.universes.len(), "universe"),
    )
    .into()
}

/// `n thing`, `n things`. One helper rather than an `s` at each call site,
/// because "1 universes" is the kind of thing nobody notices until it ships.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Everything that is about a *universe* rather than about a fixture.
fn rail(state: &Patch, app: &Entity<Luma>) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .w(px(RAIL_WIDTH))
        .h_full()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(ladder::trim())
        .child(footprint::footprint(state, app))
        .child(outputs::outputs(state, app))
        .child(groups::groups(state))
}

/// Wide enough for a 32-column footprint grid at 8 px a cell plus its gutters.
const RAIL_WIDTH: f32 = 336.0;

/// A section in the rail: a silkscreen heading over its body.
///
/// `grow` says whether it takes the room left over — one section does, the
/// others are the size of what is in them. Passed rather than chained on by
/// the caller because the automation node has to be the outermost thing, and a
/// caller that added layout after it would be sizing a box the tree does not
/// describe.
fn section(heading: &'static str, grow: bool, body: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .min_h_0()
        .map(|section| {
            if grow {
                section.flex_1()
            } else {
                section.flex_shrink_0()
            }
        })
        .border_b_1()
        .border_color(ladder::trim())
        .child(
            div()
                .px(px(12.0))
                .pt(px(12.0))
                .pb(px(8.0))
                .child(luma_ui::silkscreen(heading)),
        )
        .child(body)
        .agent_node(Role::Card, heading)
        .into_any_element()
}
