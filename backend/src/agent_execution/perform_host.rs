//! Host capabilities behind `luma.perform.*`: a venue's Perform set, its
//! bindings, and what plays now.
//!
//! The venue every call acts on comes from the durable thread's scope, and
//! only a Perform thread has one: Python names no venue id.
//!
//! # The writes are not implemented here
//!
//! Every change to the set is one [`PerformSet::change`], the same one the
//! Perform tab's commands make: the same checks (a pattern passes a score's
//! checker, a cell fits its page), one transaction, and the open set
//! recompiled after it. This file is the boundary: it decodes the payload,
//! states the verb as an edit of the set as stored, refuses what makes no
//! sense, and answers the set as it now stands, so a program reads its own
//! effect without a second call. Bindings go through the input hub the
//! Perform tab's Map mode uses, and pressing or moving a control through the
//! session's control bank, as a pad or fader on screen does.

use std::path::PathBuf;
use std::sync::Arc;

use luma_patterns::clip_graph::{is_control_name, ClipGraph};
use luma_patterns::press::TimeUnit;
use luma_patterns::BlendMode;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::runtime::Handle;

use crate::agent_execution::cell_host::{call_limit, decode, supervise};
use crate::agent_execution::worker_process::{HostCallContext, HostCallError, HostCallHandler};
use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::error::CommandError;
use crate::inputs::{InputHub, Mode};
use crate::models::perform::{
    Cell, CellContent, ControlTarget, LayoutPage, PerformLayout, PerformPattern, Step,
};
use crate::perform::PerformSession;
use crate::services::perform_set::{PerformSet, Set};
use crate::services::{busk_page, groups};

/// The most columns or rows a page has, as on the Perform tab.
const MOST: u16 = 16;

/// One cell's Perform capability table. Construct only from host-resolved
/// scope.
pub struct PerformHost {
    runtime: Handle,
    pool: SqlitePool,
    /// The fixtures root, to read the venue's groups and rig.
    resource_root: PathBuf,
    venue_id: String,
    /// Who this thread's writes are attributed to: the thread's `actor`.
    actor: Option<String>,
    session: Arc<PerformSession>,
    inputs: Arc<InputHub>,
}

impl PerformHost {
    pub(crate) fn new(
        runtime: Handle,
        pool: SqlitePool,
        resource_root: PathBuf,
        venue_id: String,
        actor: Option<String>,
        session: Arc<PerformSession>,
        inputs: Arc<InputHub>,
    ) -> Self {
        Self {
            runtime,
            pool,
            resource_root,
            venue_id,
            actor,
            session,
            inputs,
        }
    }

    fn set(&self) -> PerformSet<'_> {
        PerformSet::new(&self.pool, &self.session, &self.venue_id).acting_as(self.actor.as_deref())
    }

    /// Change the set, and answer it as it now stands with what `change`
    /// answered beside it.
    async fn change(
        &self,
        change: impl FnOnce(&mut Set) -> Result<Value, CommandError>,
    ) -> Result<Value, HostCallError> {
        let mut answer = self.set().change(change).await?;
        let set = self.set().read().await?;
        answer["set"] = set.described();
        Ok(answer)
    }

    /// One method, by name. Every write is an edit of the set as stored.
    pub(crate) async fn call(&self, method: &str, payload: Value) -> Result<Value, HostCallError> {
        match method {
            "perform.read" => Ok(json!({ "set": self.set().read().await?.described() })),
            "perform.add" => {
                let add: Add = decode(payload)?;
                self.check_selection(&add.selection).await?;
                self.change(|set| add.apply(set)).await
            }
            "perform.update" => {
                let update: Update = decode(payload)?;
                if let Some(selection) = &update.selection {
                    self.check_selection(selection).await?;
                }
                self.change(|set| update.apply(set)).await
            }
            "perform.remove" => {
                let Id { id } = decode(payload)?;
                self.change(|set| set.remove(&id).map(|_| json!({}))).await
            }
            "perform.reorder" => {
                let Reorder { ids } = decode(payload)?;
                self.change(|set| reorder(set, &ids)).await
            }
            "perform.page" => {
                let page: Page = decode(payload)?;
                self.change(|set| page.apply(&mut set.layout)).await
            }
            "perform.remove_page" => {
                let Id { id } = decode(payload)?;
                self.change(|set| remove_page(&mut set.layout, &id)).await
            }
            "perform.place" => {
                let place: Place = decode(payload)?;
                if let CellContent::Target { group, .. } = &place.content {
                    self.check_selection(group).await?;
                }
                self.change(|set| place.apply(set)).await
            }
            "perform.cell" => {
                let change: CellChange = decode(payload)?;
                if let Some(group) = &change.group {
                    self.check_selection(group).await?;
                }
                self.change(|set| change.apply(set)).await
            }
            "perform.remove_cell" => {
                let Id { id } = decode(payload)?;
                self.change(|set| remove_cell(&mut set.layout, &id)).await
            }
            "perform.busk" => {
                let rig =
                    busk_page::read_rig(&self.pool, &self.resource_root, &self.venue_id).await?;
                self.change(|set| Ok(json!({ "id": busk_page::add(set, &rig) })))
                    .await
            }
            "perform.live" => Ok(self.live()),
            "perform.control" => {
                let Control { name, value, on } = decode(payload)?;
                if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                    return Err(refused(format!("a control's value is 0 to 1; got {value}")));
                }
                self.require_open(&name)?;
                match on {
                    Some(on) => {
                        for group in &on {
                            self.check_selection(group).await?;
                        }
                        self.session.set_on(&name, value, &on)
                    }
                    None => self.session.set_control(&name, value),
                }
                .map_err(refused)?;
                Ok(self.live())
            }
            "perform.press" => {
                let Pressed { name, down } = decode(payload)?;
                self.require_open(&name)?;
                self.session.press(&name, down).map_err(refused)?;
                Ok(self.live())
            }
            "perform.bindings" => {
                self.open_inputs().await?;
                Ok(json!({ "inputs": self.inputs.state() }))
            }
            "perform.binding" => {
                let Rebind { id, mode, invert } = decode(payload)?;
                self.open_inputs().await?;
                self.inputs
                    .update(&self.venue_id, &id, mode, invert)
                    .await
                    .map_err(|error| HostCallError::new("not_found", error))?;
                Ok(json!({ "inputs": self.inputs.state() }))
            }
            "perform.unbind" => {
                let Id { id } = decode(payload)?;
                self.open_inputs().await?;
                self.inputs
                    .unbind(&self.venue_id, &id)
                    .await
                    .map_err(|error| HostCallError::new("internal", error))?;
                Ok(json!({ "inputs": self.inputs.state() }))
            }
            "perform.learn" => {
                let Learn { control, mode } = decode(payload)?;
                control_name(&control)?;
                self.open_inputs().await?;
                let binding = self
                    .inputs
                    .learn(&control, mode)
                    .await
                    .map_err(|error| HostCallError::new("not_heard", error))?;
                Ok(json!({ "binding": binding, "inputs": self.inputs.state() }))
            }
            _ => Err(HostCallError::new(
                "unknown_method",
                format!("unknown perform host method {method:?}"),
            )),
        }
    }

    /// What plays now. The control values and the clock are the open set's,
    /// so they are this venue's only while its set is open.
    fn live(&self) -> Value {
        if self.session.open_venue().as_deref() != Some(self.venue_id.as_str()) {
            return json!({ "open": false });
        }
        json!({ "open": true, "state": self.session.state() })
    }

    /// Refuse to play `name` unless it is a control name and this venue's
    /// set is the one open, which the Perform tab does while it is shown.
    fn require_open(&self, name: &str) -> Result<(), HostCallError> {
        control_name(name)?;
        if self.session.open_venue().as_deref() != Some(self.venue_id.as_str()) {
            return Err(refused(
                "this venue's Perform set is not open; it opens while its Perform tab is shown",
            ));
        }
        Ok(())
    }

    /// Bind the input hub to this venue, as the Perform tab does when it
    /// opens.
    async fn open_inputs(&self) -> Result<(), HostCallError> {
        if self.inputs.state().venue_id.as_deref() == Some(self.venue_id.as_str()) {
            return Ok(());
        }
        self.inputs
            .open(&self.venue_id)
            .await
            .map_err(|error| HostCallError::new("internal", error))
    }

    /// Refuse a selection that names a group this venue does not have: it
    /// would light nothing, and say nothing about why.
    async fn check_selection(&self, expression: &str) -> Result<(), HostCallError> {
        let mut access =
            VenueAccess::<Read>::read(&self.pool, VenueResource::Venue(&self.venue_id))
                .await
                .map_err(|error| HostCallError::new("forbidden", error))?;
        let tree = groups::GroupSources::read(&self.resource_root, &mut access)
            .await
            .map_err(|error| HostCallError::new("internal", error))?
            .tree();
        groups::validate_selection_names(expression, &tree).map_err(refused)
    }
}

impl HostCallHandler for PerformHost {
    fn handle(
        &self,
        method: &str,
        payload: Value,
        context: &HostCallContext,
    ) -> Result<Value, HostCallError> {
        context.check()?;
        if WRITES.contains(&method) {
            // A change that begins is stored, recompiled and answered: a Stop
            // lands before it or after it, never between the commit and the
            // recompile.
            context.begin_irreversible()?;
            return self.runtime.block_on(self.call(method, payload));
        }
        let limit = call_limit(context)?;
        self.runtime
            .block_on(supervise(self.call(method, payload), context, limit))
    }
}

/// The methods that change the set.
const WRITES: [&str; 10] = [
    "perform.add",
    "perform.update",
    "perform.remove",
    "perform.reorder",
    "perform.page",
    "perform.remove_page",
    "perform.place",
    "perform.cell",
    "perform.remove_cell",
    "perform.busk",
];

impl From<CommandError> for HostCallError {
    fn from(error: CommandError) -> Self {
        let message = error.to_string();
        match error {
            CommandError::Invalid(_) => refused(message),
            CommandError::NotFound(_) => HostCallError::new("not_found", message),
            _ => HostCallError::new("internal", message),
        }
    }
}

fn refused(message: impl Into<String>) -> HostCallError {
    HostCallError::new("refused", message)
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::Invalid(message.into())
}

fn control_name(name: &str) -> Result<(), HostCallError> {
    if is_control_name(name) {
        Ok(())
    } else {
        Err(refused(format!(
            "expected a control name in snake_case, such as drop or front_level; got {name:?}"
        )))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: String,
}

/// A new pattern, and its cell. Its graph gets a pad when it has none
/// ([`ClipGraph::with_pad`]), as a pattern the Perform tab makes does.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Add {
    name: String,
    selection: String,
    graph: ClipGraph,
    blend: BlendMode,
    #[serde(default)]
    seed: Option<u64>,
    /// Its place in the set; on top when absent.
    #[serde(default)]
    z: Option<i64>,
    /// The page's id; the first page when absent.
    #[serde(default)]
    page: Option<String>,
    /// `(column, row)`, from 0; the first free place when absent.
    #[serde(default)]
    at: Option<(u16, u16)>,
    #[serde(default = "one_by_one")]
    size: (u16, u16),
}

fn one_by_one() -> (u16, u16) {
    (1, 1)
}

impl Add {
    fn apply(self, set: &mut Set) -> Result<Value, CommandError> {
        let pattern = PerformPattern {
            id: uuid::Uuid::new_v4().to_string(),
            venue_id: String::new(),
            name: named(self.name)?,
            selection: luma_patterns::Selection {
                expression: self.selection,
            },
            z_index: self.z.unwrap_or_else(|| set.top()),
            blend_mode: self.blend,
            seed: self
                .seed
                .unwrap_or_else(|| uuid::Uuid::new_v4().as_u64_pair().0),
            graph: self.graph.with_pad(),
        };
        let content = CellContent::Pattern {
            pattern_id: pattern.id.clone(),
        };
        let cell = put_cell(
            &mut set.layout,
            self.page.as_deref(),
            self.at,
            self.size,
            content,
        )?;
        let id = pattern.id.clone();
        set.put(pattern);
        Ok(json!({ "id": id, "cell": cell }))
    }
}

/// A change to one pattern: what is given changes, the rest stays.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    selection: Option<String>,
    #[serde(default)]
    graph: Option<ClipGraph>,
    #[serde(default)]
    blend: Option<BlendMode>,
    #[serde(default)]
    z: Option<i64>,
    #[serde(default)]
    seed: Option<u64>,
}

impl Update {
    fn apply(self, set: &mut Set) -> Result<Value, CommandError> {
        let pattern = set.pattern_mut(&self.id)?;
        if let Some(name) = self.name {
            pattern.name = named(name)?;
        }
        if let Some(expression) = self.selection {
            pattern.selection = luma_patterns::Selection { expression };
        }
        if let Some(graph) = self.graph {
            pattern.graph = graph;
        }
        if let Some(blend) = self.blend {
            pattern.blend_mode = blend;
        }
        if let Some(z) = self.z {
            pattern.z_index = z;
        }
        if let Some(seed) = self.seed {
            pattern.seed = seed;
        }
        Ok(json!({ "id": self.id }))
    }
}

fn named(name: String) -> Result<String, CommandError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("a pattern needs a name, such as \"Warm wash\""));
    }
    Ok(name.to_owned())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reorder {
    /// Patterns top first. Those not named keep their order under them.
    ids: Vec<String>,
}

fn reorder(set: &mut Set, ids: &[String]) -> Result<Value, CommandError> {
    let mut order: Vec<PerformPattern> = Vec::with_capacity(set.patterns.len());
    for id in ids {
        if order.iter().any(|pattern| &pattern.id == id) {
            return Err(invalid(format!("pattern {id} is named twice")));
        }
        order.push(set.remove(id)?);
    }
    // What is left is still the top of the set first, as it was read.
    order.append(&mut set.patterns);
    let count = order.len() as i64;
    for (at, pattern) in order.iter_mut().enumerate() {
        pattern.z_index = count - 1 - at as i64;
    }
    set.patterns = order;
    Ok(json!({}))
}

/// A new page (no `id`), or a change to one: what is given changes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    columns: Option<u16>,
    #[serde(default)]
    rows: Option<u16>,
    /// Rows, from 0, whose patterns play one at a time.
    #[serde(default)]
    one_at_a_time: Option<Vec<u16>>,
}

impl Page {
    fn apply(self, layout: &mut PerformLayout) -> Result<Value, CommandError> {
        let page = match &self.id {
            Some(id) => page_mut(layout, Some(id))?,
            None => {
                let number = layout.pages.len() + 1;
                layout.pages.push(LayoutPage {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: format!("Page {number}"),
                    columns: 8,
                    rows: 4,
                    one_at_a_time: Vec::new(),
                    cells: Vec::new(),
                });
                layout.pages.last_mut().expect("just pushed")
            }
        };
        if let Some(name) = self.name {
            let name = name.trim();
            if name.is_empty() {
                return Err(invalid("a page needs a name"));
            }
            page.name = name.to_owned();
        }
        for (what, value, field) in [
            ("columns", self.columns, &mut page.columns),
            ("rows", self.rows, &mut page.rows),
        ] {
            if let Some(value) = value {
                if !(1..=MOST).contains(&value) {
                    return Err(invalid(format!(
                        "a page has 1 to {MOST} {what}; got {value}"
                    )));
                }
                *field = value;
            }
        }
        if let Some(mut rows) = self.one_at_a_time {
            rows.sort_unstable();
            rows.dedup();
            page.one_at_a_time = rows;
        }
        if let Some(row) = page.one_at_a_time.iter().find(|row| **row >= page.rows) {
            return Err(invalid(format!(
                "page \"{}\" has {} rows, so it has no row {row}",
                page.name, page.rows
            )));
        }
        Ok(json!({ "id": page.id }))
    }
}

fn remove_page(layout: &mut PerformLayout, id: &str) -> Result<Value, CommandError> {
    let at = layout
        .pages
        .iter()
        .position(|page| page.id == id)
        .ok_or_else(|| no_such("page", id))?;
    if layout.pages.len() == 1 {
        return Err(invalid("a set keeps at least one page"));
    }
    if layout.pages[at]
        .cells
        .iter()
        .any(|cell| cell.pattern_id().is_some())
    {
        return Err(invalid(format!(
            "page \"{}\" holds patterns: move or delete them first",
            layout.pages[at].name
        )));
    }
    layout.pages.remove(at);
    Ok(json!({}))
}

/// A new cell of the master, tap, a fader of its own, a color cell or a
/// target.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Place {
    content: CellContent,
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    at: Option<(u16, u16)>,
    #[serde(default = "one_by_one")]
    size: (u16, u16),
}

impl Place {
    fn apply(self, set: &mut Set) -> Result<Value, CommandError> {
        if let CellContent::Pattern { .. } = self.content {
            return Err(invalid(
                "a pattern's cell comes with the pattern: add the pattern instead",
            ));
        }
        let cell = put_cell(
            &mut set.layout,
            self.page.as_deref(),
            self.at,
            self.size,
            self.content,
        )?;
        Ok(json!({ "id": cell }))
    }
}

/// A cell moved, resized or taken to another page, a fader cell's label and
/// targets, a color cell's label, targets, swatches, fade and step, or a
/// target's label and group: what is given changes. A step of null stops a
/// color cell stepping.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CellChange {
    id: String,
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    at: Option<(u16, u16)>,
    #[serde(default)]
    size: Option<(u16, u16)>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    targets: Option<Vec<ControlTarget>>,
    #[serde(default)]
    swatches: Option<Vec<[f64; 3]>>,
    #[serde(default)]
    fade: Option<f64>,
    #[serde(default)]
    unit: Option<TimeUnit>,
    #[serde(default, deserialize_with = "given")]
    step: Option<Option<Step>>,
    #[serde(default)]
    group: Option<String>,
}

/// A field that was given, null or not: absent is `None`.
fn given<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

impl CellChange {
    fn apply(self, set: &mut Set) -> Result<Value, CommandError> {
        let from = set
            .layout
            .pages
            .iter()
            .position(|page| page.cells.iter().any(|cell| cell.id == self.id))
            .ok_or_else(|| no_such("cell", &self.id))?;
        let held = set.layout.pages[from]
            .cells
            .iter()
            .position(|cell| cell.id == self.id)
            .expect("found on this page");
        let mut cell = set.layout.pages[from].cells.remove(held);
        let colors = self.swatches.is_some()
            || self.fade.is_some()
            || self.unit.is_some()
            || self.step.is_some();
        if self.group.is_some() && !matches!(cell.content, CellContent::Target { .. }) {
            return Err(invalid(format!(
                "cell {} is not a target, so it has no group",
                cell.id
            )));
        }
        match &mut cell.content {
            CellContent::Target { .. } if colors || self.targets.is_some() => {
                return Err(invalid(format!(
                    "cell {} is a target: it has a label and a group",
                    cell.id
                )))
            }
            CellContent::Target { label, group } => {
                set_given(label, self.label.map(|new| new.trim().to_owned()));
                set_given(group, self.group);
            }
            CellContent::Fader { .. } if colors => {
                return Err(invalid(format!(
                    "cell {} is a fader: it has no swatches, fade or step",
                    cell.id
                )))
            }
            CellContent::Fader { label, targets } => {
                set_given(label, self.label.map(|new| new.trim().to_owned()));
                set_given(targets, self.targets);
            }
            CellContent::Color {
                label,
                swatches,
                fade,
                unit,
                targets,
                step,
            } => {
                set_given(label, self.label.map(|new| new.trim().to_owned()));
                set_given(targets, self.targets);
                set_given(swatches, self.swatches);
                set_given(fade, self.fade);
                set_given(unit, self.unit);
                set_given(step, self.step);
            }
            _ if colors || self.label.is_some() || self.targets.is_some() => {
                return Err(invalid(format!(
                    "cell {} is not a fader, a color cell or a target, so it has no label or targets",
                    cell.id
                )))
            }
            _ => {}
        }
        if let Some((width, height)) = self.size {
            cell.width = width;
            cell.height = height;
        }
        let to = match self.page.as_deref() {
            Some(id) => set
                .layout
                .pages
                .iter()
                .position(|page| page.id == id)
                .ok_or_else(|| no_such("page", id))?,
            None => from,
        };
        let page = &mut set.layout.pages[to];
        match self.at {
            Some((column, row)) => {
                cell.column = column;
                cell.row = row;
            }
            // Taken to another page with no place named: its first room.
            None if to != from => {
                let (column, row) = page
                    .free_spot(cell.width, cell.height)
                    .ok_or_else(|| no_room(page, cell.width, cell.height))?;
                cell.column = column;
                cell.row = row;
            }
            None => {}
        }
        page.cells.push(cell);
        Ok(json!({ "id": self.id }))
    }
}

fn set_given<T>(field: &mut T, given: Option<T>) {
    if let Some(given) = given {
        *field = given;
    }
}

/// Remove a cell of the master, tap, a fader or a color cell. A pattern's
/// cell goes with its pattern.
fn remove_cell(layout: &mut PerformLayout, id: &str) -> Result<Value, CommandError> {
    let page = layout
        .pages
        .iter_mut()
        .find(|page| page.cells.iter().any(|cell| cell.id == id))
        .ok_or_else(|| no_such("cell", id))?;
    if page
        .cells
        .iter()
        .any(|cell| cell.id == id && cell.pattern_id().is_some())
    {
        return Err(invalid(
            "a pattern's cell goes with its pattern: remove the pattern instead",
        ));
    }
    page.cells.retain(|cell| cell.id != id);
    Ok(json!({}))
}

/// Put a new cell holding `content` on `page` (the first when `None`), at
/// `at` or in the first free place. Answers its id. Whether it fits where it
/// was put is checked with the whole layout when the change is stored.
fn put_cell(
    layout: &mut PerformLayout,
    page: Option<&str>,
    at: Option<(u16, u16)>,
    (width, height): (u16, u16),
    content: CellContent,
) -> Result<String, CommandError> {
    let page = page_mut(layout, page)?;
    let (column, row) = match at {
        Some(at) => at,
        None => page
            .free_spot(width, height)
            .ok_or_else(|| no_room(page, width, height))?,
    };
    let cell = Cell {
        id: uuid::Uuid::new_v4().to_string(),
        column,
        row,
        width,
        height,
        content,
    };
    let id = cell.id.clone();
    page.cells.push(cell);
    Ok(id)
}

fn page_mut<'a>(
    layout: &'a mut PerformLayout,
    id: Option<&str>,
) -> Result<&'a mut LayoutPage, CommandError> {
    match id {
        Some(id) => layout
            .pages
            .iter_mut()
            .find(|page| page.id == id)
            .ok_or_else(|| no_such("page", id)),
        None => layout
            .pages
            .first_mut()
            .ok_or_else(|| invalid("the set has no page yet: add one first")),
    }
}

fn no_such(what: &str, id: &str) -> CommandError {
    CommandError::NotFound(format!("this Perform set has no {what} {id}"))
}

fn no_room(page: &LayoutPage, width: u16, height: u16) -> CommandError {
    invalid(format!(
        "page \"{}\" has no room for a {width} by {height} cell: make it bigger or clear a place",
        page.name
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    name: String,
    value: f64,
    /// For a pad: the venue groups to play its pattern on, whatever targets
    /// are held; none is its own selection.
    #[serde(default)]
    on: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pressed {
    name: String,
    down: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rebind {
    id: String,
    mode: Mode,
    invert: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Learn {
    control: String,
    mode: Mode,
}

#[cfg(test)]
mod tests;
