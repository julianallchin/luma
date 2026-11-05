//! Wire types for live follow: which DJ source to follow, and what the
//! session following it currently knows.

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
    /// The venue's controls as set now, the Perform set's own among them
    /// (`controls::perform`).
    pub controls: std::collections::BTreeMap<String, f64>,
    /// The open set's buttons as they stand now, for a reader to play their
    /// envelopes on at the set's tempo.
    pub buttons: std::collections::BTreeMap<String, luma_patterns::press::Button>,
}

impl PerformState {
    pub(crate) fn off(
        latency_ms: f32,
        clock: Option<PerformClock>,
        controls: std::collections::BTreeMap<String, f64>,
        buttons: std::collections::BTreeMap<String, luma_patterns::press::Button>,
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
            controls,
            buttons,
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
            start,
            duration,
            seed: self.seed,
            selection_seed: None,
            selection: self.selection.clone(),
            z_index: self.z_index,
            blend_mode: self.blend_mode,
            graph: self.graph.clone(),
        }
    }
}

/// How a venue's Perform tab lays out its set: named pages of a grid. A cell
/// holds one pattern (its level, and a fader or button for each fader and
/// button node of its graph, its pad first), or the master, tap, or a fader
/// of its own. Nothing is tied to a controller, which reaches a cell through
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
    /// row's others off.
    #[serde(default)]
    pub one_at_a_time: Vec<u16>,
    pub cells: Vec<Cell>,
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
    /// A fader of its own, moving every one of `targets` together: one knob
    /// on three chases' speeds.
    #[serde(rename_all = "camelCase")]
    Fader {
        label: String,
        targets: Vec<ControlTarget>,
    },
}

/// Something a fader cell moves.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ControlTarget {
    /// A pattern's level.
    #[serde(rename_all = "camelCase")]
    Level { pattern_id: String },
    /// A fader node of a pattern's graph, by node id.
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

    /// Why this layout cannot be stored, if it cannot: a cell off its page or
    /// over another.
    pub fn check(&self) -> Result<(), String> {
        match self
            .pages
            .iter()
            .find_map(|page| page.cells.iter().find_map(|cell| page.misfit(cell)))
        {
            Some(misfit) => Err(misfit),
            None => Ok(()),
        }
    }

    /// Take pattern `id` off the layout: its cell, and every fader cell's
    /// hold on it.
    pub fn forget(&mut self, id: &str) {
        for page in &mut self.pages {
            page.cells.retain(|cell| cell.pattern_id() != Some(id));
            for cell in &mut page.cells {
                if let CellContent::Fader { targets, .. } = &mut cell.content {
                    targets.retain(|target| match target {
                        ControlTarget::Level { pattern_id }
                        | ControlTarget::Node { pattern_id, .. } => pattern_id != id,
                        ControlTarget::Named { .. } => true,
                    });
                }
            }
        }
    }

    /// Every cell holding `pattern_id`, on any page.
    pub fn cells_of<'a>(&'a self, pattern_id: &'a str) -> impl Iterator<Item = &'a Cell> + 'a {
        self.pages
            .iter()
            .flat_map(|page| page.cells.iter())
            .filter(move |cell| cell.pattern_id() == Some(pattern_id))
    }
}
