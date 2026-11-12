//! Wire types for live follow: which DJ source to follow, and what the
//! session following it currently knows.

use luma_patterns::clip_graph::{is_control_name, LiveInputKind};
use serde::{Deserialize, Serialize};

/// A DJ source live follow can listen to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PerformSource {
    /// Rekordbox, through rkbx_link's OSC output. `address` is where to
    /// listen, rkbx_link's `osc.destination`; `127.0.0.1:4460` when absent.
    RkbxLink {
        #[serde(default)]
        address: Option<String>,
    },
    /// Pioneer CDJs. `device_num` is the virtual player number Luma claims,
    /// which must not collide with a real player on the network.
    ProDjLink { device_num: u8 },
    /// Denon / Engine DJ hardware.
    StageLinq,
    /// Luma's own player as a deck, to practise and see patterns without DJ
    /// gear: whatever library track the player has loaded.
    Virtual,
}

/// Everything live follow knows, as of the instant it was read. Positions are
/// extrapolated to that instant.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformState {
    /// The source being followed, or `None` when live follow is off.
    pub source: Option<PerformSource>,
    pub venue_id: Option<String>,
    /// Whether the source is up: listening or connected.
    pub connected: bool,
    /// What the source is doing, for a status line ("listening on …").
    pub detail: String,
    /// Milliseconds since the source last sent anything.
    pub last_heard_ms: Option<u64>,
    /// Why following may not work, when something is wrong.
    pub problem: Option<String>,
    /// Shifts every deck's lights earlier (positive) or later, to cover the
    /// source's and the DJ software's output latency.
    pub latency_ms: f32,
    pub decks: Vec<PerformDeck>,
    /// The beat the Perform set plays on: the master deck's, or tapped.
    /// `None` while no set is open.
    pub clock: Option<PerformClock>,
    /// What the open set's venue's control bank plays now: its controls as
    /// set, the Perform set's own among them (`controls::perform`), its
    /// buttons, live colors, lists, groups, Select and base colors, and
    /// where each effect plays.
    #[serde(flatten)]
    pub bank: crate::controls::Playing,
}

impl PerformState {
    pub(crate) fn off(
        latency_ms: f32,
        clock: Option<PerformClock>,
        bank: crate::controls::Playing,
    ) -> Self {
        Self {
            source: None,
            venue_id: None,
            connected: false,
            detail: String::new(),
            last_heard_ms: None,
            problem: None,
            latency_ms,
            decks: Vec::new(),
            clock,
            bank,
        }
    }
}

/// What the Perform set's patterns are timed by.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PerformClock {
    /// The master deck's song, on that song's beat grid.
    Deck { deck: u8 },
    /// The tapped tempo, for a set with no DJ software: a live band.
    Tap(Tempo),
}

/// A tempo and where its beat is.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tempo {
    pub bpm: f64,
    /// Beats since the tempo started, fractional.
    pub beat: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformDeck {
    /// Numbered from 1, as players and mixers are.
    pub id: u8,
    pub title: String,
    pub artist: String,
    /// The deck's current (pitched) tempo.
    pub bpm: f64,
    /// The playhead, in track seconds.
    pub position: f64,
    pub playing: bool,
    pub track: DeckMatch,
}

/// How a deck's track resolved against the library.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DeckMatch {
    /// The source has not said what is loaded.
    Unknown,
    Matching,
    /// `has_score` is false when the track has no score in this venue, so the
    /// deck drives nothing. `manual` marks a track picked by hand.
    #[serde(rename_all = "camelCase")]
    Matched {
        track_id: String,
        title: String,
        artist: String,
        has_score: bool,
        manual: bool,
    },
    Unmatched,
    /// Matching or loading the scene failed.
    Failed {
        message: String,
    },
}

/// A pattern in a venue's Perform set: the same graph, target, blend and seed
/// a score's pattern has, without a place on a timeline. It lives in one cell
/// of the venue's layout. Its pad is a button node of its graph, the one its
/// output's `alpha` depends on ([`luma_patterns::ClipGraph::pad`]), so the
/// pad's envelope is in the graph's alpha; the set scales that by the
/// pattern's level × the Perform master. A pattern with no pad plays at its
/// level whenever the set is open.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformPattern {
    pub id: String,
    pub venue_id: String,
    pub name: String,
    pub selection: luma_patterns::Selection,
    /// Its place in the set: where two patterns write the same attribute of
    /// the same light, the higher one wins.
    pub z_index: i64,
    pub blend_mode: luma_patterns::BlendMode,
    pub seed: u64,
    pub graph: luma_patterns::clip_graph::ClipGraph,
}

impl PerformPattern {
    /// Why this pattern cannot be stored, if it cannot: an id that is not
    /// letters, digits and dashes, or a pattern a score would refuse.
    pub fn check(&self) -> Result<(), String> {
        let id_fits = !self.id.is_empty()
            && self.id.len() <= 50
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !id_fits {
            return Err(format!(
                "expected a pattern id of letters, digits and dashes, such as a UUID; got \"{}\"",
                self.id
            ));
        }
        luma_patterns::Score::validate_clip(
            &luma_patterns::standard_library(),
            &self.id,
            &self.as_clip(0.0, 1.0),
        )
        .map_err(|error| error.to_string())
    }

    /// The pattern as a score would hold it, from `start` for `duration`
    /// beats.
    pub fn as_clip(&self, start: f64, duration: f64) -> luma_patterns::Clip {
        luma_patterns::Clip {
            name: self.name.clone(),
            label: None,
            start,
            duration,
            seed: self.seed,
            selection: self.selection.clone(),
            z_index: self.z_index,
            blend_mode: self.blend_mode,
            graph: self.graph.clone(),
        }
    }
}

/// How a venue's Perform tab lays out its set: named pages of a grid. A cell
/// holds one pattern (its level, and a fader, color or button for each live
/// node of its graph, its pad first), a venue group to choose, a list of
/// values, a fader of its own, or a utility: the master, tap, Select or
/// Clear. Nothing is tied to a controller, which reaches a cell through
/// bindings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformLayout {
    pub pages: Vec<LayoutPage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutPage {
    pub id: String,
    pub name: String,
    pub columns: u16,
    pub rows: u16,
    /// Rows whose patterns play one at a time: turning one on turns the
    /// row's others off where it plays.
    #[serde(default)]
    pub one_at_a_time: Vec<u16>,
    #[serde(deserialize_with = "known_cells")]
    pub cells: Vec<Cell>,
}

/// The cells of a stored page, less any of a kind this build does not
/// know, such as a target cell of an older one: it is dropped as the page
/// is read.
fn known_cells<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Cell>, D::Error> {
    let cells = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(cells
        .into_iter()
        .filter_map(|cell| match serde_json::from_value(cell) {
            Ok(cell) => Some(cell),
            Err(error) => {
                log::warn!("a Perform cell was dropped: {error}");
                None
            }
        })
        .collect())
}

/// A cell: the grid cells it covers and what it holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    pub id: String,
    pub column: u16,
    pub row: u16,
    pub width: u16,
    pub height: u16,
    pub content: CellContent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CellContent {
    /// A pattern of the set, which this cell is the home of.
    #[serde(rename_all = "camelCase")]
    Pattern { pattern_id: String },
    /// The level every Perform pattern is scaled by.
    Master,
    /// Tap tempo.
    Tap,
    /// Held while effects are pressed, or tapped to latch, it chooses them
    /// instead of turning them on or off, for a color to set.
    Select,
    /// Tapped, nothing is chosen; held, every effect is let go.
    Clear,
    /// A fader of its own, moving every one of `targets` together: one knob
    /// on three chases' speeds.
    #[serde(rename_all = "camelCase")]
    Fader {
        label: String,
        targets: Vec<ControlTarget>,
    },
    /// Values of one kind, colors or numbers, one of which is on. Tapping
    /// one sets every one of `targets` to it, a color fading over `fade`
    /// (in `unit`). A color list with no targets sets the base color of the
    /// groups chosen, or after Select the color of the effects chosen.
    /// Which value is on is live, in the control bank.
    #[serde(rename_all = "camelCase")]
    List {
        label: String,
        values: Vec<ListValue>,
        fade: f64,
        unit: luma_patterns::press::TimeUnit,
        targets: Vec<ControlTarget>,
        /// Moving on to the next value by itself, on the set's bars.
        #[serde(default)]
        step: Option<Step>,
    },
    /// A venue group, by id (or `all`), to choose: tapped, it is chosen
    /// or not; held, what is pressed meanwhile acts on it alone. An empty
    /// label shows the group's.
    #[serde(rename_all = "camelCase")]
    Group { label: String, group: String },
}

/// One value of a list cell.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListValue {
    /// Linear Rec. 2020 rgb, each channel 0–1.
    Color([f64; 3]),
    /// 0–1, as a fader sets.
    Number(f64),
    /// The color below: an effect chosen by Select takes it again.
    Below,
}

impl ListValue {
    /// Whether this is of a color list: a color, or the color below.
    pub fn is_color(self) -> bool {
        !matches!(self, ListValue::Number(_))
    }
}

/// A list cell moving on by itself: to the next value every `bars` bars of
/// the set's clock, after the last back to the first when it `loops`, else
/// staying on the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub bars: u16,
    pub loops: bool,
}

/// The most values a list cell holds: a controller's 4 × 4 pads.
pub const MOST_VALUES: usize = 16;

/// Something a fader cell moves, or a list cell sets.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ControlTarget {
    /// A pattern's level.
    #[serde(rename_all = "camelCase")]
    Level { pattern_id: String },
    /// A fader or live color node of a pattern's graph, by node id.
    #[serde(rename_all = "camelCase")]
    Node { pattern_id: String, node: String },
    /// One of the venue's own controls, which score patterns read by name.
    Named { name: String },
}

impl Cell {
    fn overlaps(&self, other: &Cell) -> bool {
        self.column < other.column + other.width
            && other.column < self.column + self.width
            && self.row < other.row + other.height
            && other.row < self.row + self.height
    }

    /// The pattern this cell holds, if it holds one.
    pub fn pattern_id(&self) -> Option<&str> {
        match &self.content {
            CellContent::Pattern { pattern_id } => Some(pattern_id),
            _ => None,
        }
    }
}

impl CellContent {
    /// What a fader or list cell moves; nothing for any other cell.
    pub fn targets(&self) -> &[ControlTarget] {
        match self {
            CellContent::Fader { targets, .. } | CellContent::List { targets, .. } => targets,
            _ => &[],
        }
    }

    /// What a fader or list cell moves, to change; `None` for any other
    /// cell.
    pub fn targets_mut(&mut self) -> Option<&mut Vec<ControlTarget>> {
        match self {
            CellContent::Fader { targets, .. } | CellContent::List { targets, .. } => Some(targets),
            _ => None,
        }
    }

    /// Whether this is a list of colors, whose targets are live colors.
    pub fn colors(&self) -> bool {
        matches!(self, CellContent::List { values, .. } if values.first().is_some_and(|v| v.is_color()))
    }

    /// Why this cannot be stored with `patterns`, if it cannot: a fader
    /// moving what is no number, a list mixing colors and numbers, setting
    /// what is not of its kind, or with its values, fade or step out of
    /// range.
    fn misfit(&self, patterns: &[PerformPattern]) -> Option<String> {
        if let CellContent::List {
            values,
            fade,
            step,
            targets,
            ..
        } = self
        {
            if values.is_empty() || values.len() > MOST_VALUES {
                return Some(format!(
                    "a list holds 1 to {MOST_VALUES} values; got {}",
                    values.len()
                ));
            }
            if values
                .iter()
                .any(|value| value.is_color() != values[0].is_color())
            {
                return Some("a list holds colors or numbers, not both".into());
            }
            let outside = |value: &ListValue| match value {
                ListValue::Color(rgb) => rgb.iter().any(|c| !(0.0..=1.0).contains(c)),
                ListValue::Number(number) => !(0.0..=1.0).contains(number),
                ListValue::Below => false,
            };
            if let Some(value) = values.iter().find(|value| outside(value)) {
                return Some(format!(
                    "a list's value is a color with each channel 0 to 1, or a number 0 to 1; got {value:?}"
                ));
            }
            if !targets.is_empty() && values.contains(&ListValue::Below) {
                return Some(
                    "the color below is for a list that sets no targets: after Select it puts an effect back on it"
                        .into(),
                );
            }
            if !fade.is_finite() || *fade < 0.0 {
                return Some(format!("a list's fade is 0 or more; got {fade}"));
            }
            if step.is_some_and(|step| step.bars == 0) {
                return Some("a list steps every 1 bar or more".into());
            }
        }
        let colors = self.colors();
        self.targets()
            .iter()
            .find_map(|target| target.misfit(colors, patterns))
    }
}

impl ControlTarget {
    /// Why a fader or number list (or with `colors`, a color list) cannot
    /// move this among `patterns`, if it cannot.
    fn misfit(&self, colors: bool, patterns: &[PerformPattern]) -> Option<String> {
        let pattern = |id: &str| patterns.iter().find(|pattern| pattern.id == id);
        match self {
            ControlTarget::Level { pattern_id } if colors => Some(format!(
                "a color list sets live colors; pattern {pattern_id}'s level is a number"
            )),
            ControlTarget::Level { pattern_id } => pattern(pattern_id)
                .is_none()
                .then(|| format!("this Perform set has no pattern {pattern_id}")),
            ControlTarget::Node { pattern_id, node } => {
                let Some(pattern) = pattern(pattern_id) else {
                    return Some(format!("this Perform set has no pattern {pattern_id}"));
                };
                let kind = pattern
                    .graph
                    .live_inputs()
                    .into_iter()
                    .find(|input| &input.node == node)
                    .map(|input| input.kind);
                let (fits, what) = if colors {
                    (kind == Some(LiveInputKind::Color), "live color")
                } else {
                    (kind == Some(LiveInputKind::Fader), "fader")
                };
                (!fits).then(|| format!("pattern \"{}\" has no {what} node {node}", pattern.name))
            }
            ControlTarget::Named { name } if colors => Some(format!(
                "a color list sets live colors of the set's patterns; got the venue's control {name}"
            )),
            ControlTarget::Named { name } => (!is_control_name(name)
                || crate::controls::perform::owns(name))
            .then(|| {
                format!("expected one of the venue's own control names, such as drop; got {name:?}")
            }),
        }
    }
}

impl LayoutPage {
    /// Why `cell` cannot sit on this page where it says, if it cannot: off
    /// the page, or over another cell. The cell itself, by id, is not in its
    /// own way.
    pub fn misfit(&self, cell: &Cell) -> Option<String> {
        if cell.width == 0
            || cell.height == 0
            || cell.column + cell.width > self.columns
            || cell.row + cell.height > self.rows
        {
            return Some(format!(
                "cell {} does not fit page \"{}\" ({}×{})",
                cell.id, self.name, self.columns, self.rows
            ));
        }
        self.cells
            .iter()
            .find(|other| other.id != cell.id && other.overlaps(cell))
            .map(|other| {
                format!(
                    "cells {} and {} overlap on page \"{}\"",
                    cell.id, other.id, self.name
                )
            })
    }

    /// The first free place, row by row, for a cell `width` × `height`.
    pub fn free_spot(&self, width: u16, height: u16) -> Option<(u16, u16)> {
        let probe = |column, row| Cell {
            id: String::new(),
            column,
            row,
            width,
            height,
            content: CellContent::Master,
        };
        (0..self.rows)
            .flat_map(|row| (0..self.columns).map(move |column| (column, row)))
            .find(|(column, row)| self.misfit(&probe(*column, *row)).is_none())
    }
}

impl PerformLayout {
    /// A first page: a cell per pattern, row by row, with tap and the master
    /// at the right.
    pub fn generated(patterns: &[PerformPattern]) -> Self {
        let columns: u16 = 8;
        let rows = (patterns.len() as u16).div_ceil(columns - 1).max(4);
        let cell = |column: u16, row: u16, height: u16, content| Cell {
            id: uuid::Uuid::new_v4().to_string(),
            column,
            row,
            width: 1,
            height,
            content,
        };
        let mut cells: Vec<Cell> = patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                let index = index as u16;
                cell(
                    index % (columns - 1),
                    index / (columns - 1),
                    1,
                    CellContent::Pattern {
                        pattern_id: pattern.id.clone(),
                    },
                )
            })
            .collect();
        cells.push(cell(columns - 1, 0, 1, CellContent::Tap));
        cells.push(cell(columns - 1, 1, 3, CellContent::Master));
        Self {
            pages: vec![LayoutPage {
                id: uuid::Uuid::new_v4().to_string(),
                name: "Main".into(),
                columns,
                rows,
                one_at_a_time: Vec::new(),
                cells,
            }],
        }
    }

    /// Why this layout cannot be stored with `patterns`, if it cannot: a
    /// cell off its page or over another, or a fader or list cell that
    /// moves what it cannot.
    pub fn check(&self, patterns: &[PerformPattern]) -> Result<(), String> {
        let misfit = self.pages.iter().find_map(|page| {
            page.cells.iter().find_map(|cell| {
                page.misfit(cell).or_else(|| {
                    cell.content
                        .misfit(patterns)
                        .map(|why| format!("cell {}: {why}", cell.id))
                })
            })
        });
        match misfit {
            Some(misfit) => Err(misfit),
            None => Ok(()),
        }
    }

    /// Drop each target that no longer fits `patterns` (its pattern or node
    /// gone, or the node now of the other kind) where `before` had it
    /// already, so a change is never refused for what it did not ask.
    pub fn tidy(&mut self, before: &PerformLayout, patterns: &[PerformPattern]) {
        let held: std::collections::HashSet<(&str, &ControlTarget)> = before
            .pages
            .iter()
            .flat_map(|page| page.cells.iter())
            .flat_map(|cell| {
                cell.content
                    .targets()
                    .iter()
                    .map(move |target| (cell.id.as_str(), target))
            })
            .collect();
        for cell in self.pages.iter_mut().flat_map(|page| page.cells.iter_mut()) {
            let colors = cell.content.colors();
            let id = cell.id.clone();
            if let Some(targets) = cell.content.targets_mut() {
                targets.retain(|target| {
                    target.misfit(colors, patterns).is_none()
                        || !held.contains(&(id.as_str(), target))
                });
            }
        }
    }

    /// Take pattern `id` off the layout: its cell, and every fader and list
    /// cell's hold on it.
    pub fn forget(&mut self, id: &str) {
        for page in &mut self.pages {
            page.cells.retain(|cell| cell.pattern_id() != Some(id));
            for cell in &mut page.cells {
                if let Some(targets) = cell.content.targets_mut() {
                    targets.retain(|target| match target {
                        ControlTarget::Level { pattern_id }
                        | ControlTarget::Node { pattern_id, .. } => pattern_id != id,
                        ControlTarget::Named { .. } => true,
                    });
                }
            }
        }
    }

    /// Whether a group cell, on any page, holds `group`.
    pub fn has_group(&self, group: &str) -> bool {
        self.pages.iter().flat_map(|page| &page.cells).any(
            |cell| matches!(&cell.content, CellContent::Group { group: held, .. } if held == group),
        )
    }

    /// Every cell holding `pattern_id`, on any page.
    pub fn cells_of<'a>(&'a self, pattern_id: &'a str) -> impl Iterator<Item = &'a Cell> + 'a {
        self.pages
            .iter()
            .flat_map(|page| page.cells.iter())
            .filter(move |cell| cell.pattern_id() == Some(pattern_id))
    }
}
