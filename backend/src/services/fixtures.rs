//! Business logic for fixture operations.
//!
//! Database layer handles CRUD only. File/resource access, DMX output refresh, and
//! in-memory fixture index live here.

use std::path::Path;
use std::sync::Mutex;

use crate::fixtures::parser::{self, FixtureIndex};
use crate::models::fixtures::FixtureEntry;
use crate::services::group_derivation;

/// In-memory fixture-definition index. `None` until [`initialize_fixtures`]
/// builds it.
pub struct FixtureState(pub Mutex<Option<FixtureIndex>>);

impl FixtureState {
    /// An un-indexed state. `initialize_fixtures` fills it.
    #[must_use]
    pub fn empty() -> Self {
        Self(Mutex::new(None))
    }

    /// Whether the index has been built. What lets a reader build it on demand
    /// instead of handing its caller an error nobody can act on.
    #[must_use]
    pub fn is_indexed(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
}

/// Initialize the fixture library (file-system side)
pub async fn initialize_fixtures(root: &Path, state: &FixtureState) -> Result<usize, String> {
    let index = catalogue(root)?;
    let count = index.entries.len();
    *state.0.lock().unwrap() = Some(index);
    Ok(count)
}

/// Every fixture a venue can be given: the Luma fixture library's, then the
/// QLC+ files under `root` that no library fixture stands for.
///
/// A library fixture that names a QLC+ file among its aliases replaces that
/// file in search, so one product is offered once and offered as the library
/// fixture. The file itself stays where it is: venues patched with it keep
/// loading it. A tree with no library beside it is searched as QLC+ alone.
///
/// # Errors
/// Fails only if `root` cannot be walked or the library's index is unreadable.
pub fn catalogue(root: &Path) -> Result<FixtureIndex, String> {
    let qlc = parser::build_index(root).map_err(|e| e.to_string())?;
    let Some(library_root) = crate::resources::fixture_library_root(Some(root)) else {
        return Ok(qlc);
    };
    let index = crate::fixtures::library::index(&library_root)?;
    let aliased: std::collections::HashSet<&str> = index
        .fixtures
        .iter()
        .filter_map(|entry| entry.aliases.as_ref()?.qlc_path.as_deref())
        .collect();
    let mut entries: Vec<FixtureEntry> = index
        .fixtures
        .iter()
        .map(|entry| FixtureEntry {
            manufacturer: entry.manufacturer.clone(),
            model: entry.model.clone(),
            path: crate::fixtures::library::fixture_path(&entry.id),
            library: Some(entry.clone()),
        })
        .collect();
    entries.extend(
        qlc.entries
            .into_iter()
            .filter(|entry| !aliased.contains(entry.path.as_str())),
    );
    entries.sort_by(|a, b| {
        a.manufacturer
            .to_lowercase()
            .cmp(&b.manufacturer.to_lowercase())
            .then(a.model.to_lowercase().cmp(&b.model.to_lowercase()))
    });
    Ok(FixtureIndex { entries })
}

/// Search for fixtures in the library
pub fn search_fixtures(
    query: String,
    offset: usize,
    limit: usize,
    state: &FixtureState,
) -> Result<Vec<FixtureEntry>, String> {
    let state_guard = state.0.lock().unwrap();

    let index = state_guard
        .as_ref()
        .ok_or("Fixtures not initialized. Call initialize_fixtures first.")?;

    Ok(best_first(&index.entries, &query)
        .into_iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect())
}

/// Whether one library entry answers `query`.
///
/// The one match rule, shared by the patch page's search field and the agent's
/// `luma.venue.fixtures`: every whitespace-separated term must appear somewhere
/// in "manufacturer model", case-insensitively. Terms rather than a substring
/// because the two names are written in one order in the file and the other in
/// a person's head — "rogue spot" and "spot rogue" find the same fixture, and
/// an empty query matches everything.
#[must_use]
fn matches(entry: &FixtureEntry, query: &str) -> bool {
    let haystack = format!("{} {}", entry.manufacturer, entry.model).to_lowercase();
    query
        .split_whitespace()
        .all(|term| haystack.contains(&term.to_lowercase()))
}

/// Every entry that answers `query`, nearest answer first.
///
/// [`matches`] says *whether* an entry answers; this says **how well**, because
/// a substring match has no sense of a word and "robe" found Martin's Strobe
/// before it found anything Robe makes. A term that starts a word beats one
/// buried inside one, and the maker's name beats the model's — searching for a
/// manufacturer is the commonest thing anybody types.
///
/// Among entries as near, a library fixture comes before a QLC+ file: it is
/// the one Luma can draw and drive in full.
///
/// A stable sort, so entries that score alike keep the index's own order.
#[must_use]
pub fn best_first<'a>(entries: &'a [FixtureEntry], query: &str) -> Vec<&'a FixtureEntry> {
    let mut found: Vec<&FixtureEntry> = entries
        .iter()
        .filter(|entry| matches(entry, query))
        .collect();
    if query.split_whitespace().next().is_some() {
        found.sort_by_key(|entry| (distance(entry, query), entry.library.is_none()));
    }
    found
}

/// How far one entry is from a query — lower is nearer. See [`best_first`].
fn distance(entry: &FixtureEntry, query: &str) -> u32 {
    let maker = entry.manufacturer.to_lowercase();
    let model = entry.model.to_lowercase();
    query
        .split_whitespace()
        .map(|term| {
            let term = term.to_lowercase();
            if starts_a_word(&maker, &term) {
                0
            } else if starts_a_word(&model, &term) {
                1
            } else {
                2
            }
        })
        .sum()
}

/// Whether `term` begins a word of `text` — the start, or after a separator.
fn starts_a_word(text: &str, term: &str) -> bool {
    text.match_indices(term).any(|(at, _)| {
        at == 0
            || text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_alphanumeric())
    })
}

/// One mode of a library fixture, as a caller choosing one needs to see it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryMode {
    /// The exact string `distribute` takes as its `mode`.
    pub name: String,
    pub channels: usize,
    /// Whether *this* mode patches a pan or a tilt — see
    /// [`crate::services::group_derivation::aims`]. A mover in a stripped-down
    /// mode does not move.
    pub moves: bool,
    /// What the derivation will file this mode under: `wash`, `spot`, `beam`,
    /// `strobe`, `blinder`, `pixel`, `fx`, `other`.
    pub role: String,
    /// How many cells it has: what `"<fixture>:<cell>"` counts, from 0.
    pub cells: usize,
    /// The emitter colours its channels drive (`red`, `warm_white`, `uv`,
    /// ...), in channel order. Empty for a mode whose colour comes from a
    /// lamp, flags or a wheel, and for a QLC+ file.
    pub colours: Vec<String>,
    /// Which cells make which colours: each distinct set of emitter colours
    /// (`"red+green+blue"`, `"white"`) and the cells, from 0, that have
    /// exactly it. Empty for a QLC+ file.
    pub cell_colours: std::collections::BTreeMap<String, Vec<usize>>,
    /// Whether Luma can light it in this mode.
    pub supported: bool,
}

/// A library fixture resolved far enough to be named in a `distribute` call.
///
/// The projection the agent surface hands out: everything needed to pick a
/// fixture and a mode, and nothing needed only to drive DMX. The channel list
/// itself stays behind — a caller choosing between "8 Channel" and "18 Channel"
/// wants the count, and the parse is thirty kilobytes.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryFixture {
    /// The `fixture` argument of `distribute`: a QLC+ file relative to the
    /// fixtures root, or `lib:<id>` for a library fixture.
    pub path: String,
    pub manufacturer: String,
    pub model: String,
    /// The definition's own QLC+ `Type` string, free text.
    pub kind: String,
    /// Whether any mode of this fixture aims.
    pub moves: bool,
    /// Lens half-range in degrees, `(min, max)`, when the definition measures
    /// one. Absent on the many QLC+ files that do not.
    pub beam_deg: Option<(f32, f32)>,
    pub modes: Vec<LibraryMode>,
    /// How well a library fixture has been checked (geometry, and how each
    /// mode was settled). Absent on a QLC+ file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<crate::fixtures::library::Quality>,
}

/// The fixtures an agent can name, `query` matched by [`matches`].
///
/// Reads the same index the patch page searches — [`catalogue`] is a
/// directory walk and one index file, not a parse — and then parses only the page it returns, so
/// naming a fixture costs one file read per row rather than one per library.
/// A definition that will not parse is dropped rather than failing the call:
/// the answer to "what can I use" must not be an error because of a file
/// nobody asked about.
///
/// # Errors
/// Fails only if the fixtures root cannot be walked.
pub fn library(root: &Path, query: &str, limit: usize) -> Result<Vec<LibraryFixture>, String> {
    let index = catalogue(root)?;
    Ok(best_first(&index.entries, query)
        .into_iter()
        .filter_map(|entry| resolve_entry(root, entry))
        .take(limit)
        .collect())
}

/// The emitter colours `mode`'s channels drive, each once, in channel order.
fn emitter_colours(
    definition: &crate::models::fixtures::FixtureDefinition,
    mode: &crate::models::fixtures::Mode,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for channel in &mode.channels {
        let Some(lowered) = definition
            .channels
            .iter()
            .find(|c| c.name == channel.name)
            .and_then(|c| c.lowered.as_deref())
        else {
            continue;
        };
        if let Some(colour) = luma_fixture::EmitterColour::of(lowered.param.attr) {
            let name = snake(&format!("{colour:?}"));
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// For each cell of `mode`, the emitter colours its channels drive there and
/// its dies can make (or the firmware emulates), grouped by identical sets.
fn cell_colours(
    definition: &crate::models::fixtures::FixtureDefinition,
    mode: &crate::models::fixtures::Mode,
) -> std::collections::BTreeMap<String, Vec<usize>> {
    use luma_fixture::{CellEmitters, EmitterColour};
    let mut out = std::collections::BTreeMap::new();
    let Some(library) = definition.library.as_ref() else {
        return out;
    };
    let built = &library.built;
    let params: Vec<&luma_fixture::control::Param> = mode
        .channels
        .iter()
        .filter_map(|channel| {
            definition
                .channels
                .iter()
                .find(|c| c.name == channel.name)
                .and_then(|c| c.lowered.as_deref())
                .filter(|lowered| lowered.rank == 0)
                .map(|lowered| &*lowered.param)
        })
        .collect();
    for cell in 0..built.cells.len() {
        let dies = match built.cell_emitters(cell) {
            CellEmitters::Known(set) => Some(set),
            _ => None,
        };
        let mut colours: Vec<String> = Vec::new();
        for param in &params {
            let covers = param.cell.is_none_or(|span| {
                let [a, z] = span.span();
                (a as usize..=z as usize).contains(&cell)
            });
            let Some(colour) = EmitterColour::of(param.attr).filter(|_| covers) else {
                continue;
            };
            if !(param.emulated || dies.is_none_or(|d| d.lit_by(colour))) {
                continue;
            }
            let name = snake(&format!("{colour:?}"));
            if !colours.contains(&name) {
                colours.push(name);
            }
        }
        if !colours.is_empty() {
            out.entry(colours.join("+"))
                .or_insert_with(Vec::new)
                .push(cell);
        }
    }
    out
}

/// `WarmWhite` as `warm_white`.
fn snake(camel: &str) -> String {
    let mut out = String::new();
    for (i, c) in camel.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// One library entry with its definition read, or `None` if it will not parse.
fn resolve_entry(root: &Path, entry: &FixtureEntry) -> Option<LibraryFixture> {
    let definition = crate::fixtures::definition(root, &entry.path).ok()?;
    let modes: Vec<LibraryMode> = definition
        .modes
        .iter()
        .map(|mode| LibraryMode {
            name: mode.name.clone(),
            channels: mode.channels.len(),
            moves: group_derivation::aims(&definition, mode),
            role: crate::models::groups::FixtureRole::of(&definition, mode)
                .as_str()
                .to_string(),
            cells: mode.heads.len().max(1),
            colours: emitter_colours(&definition, mode),
            cell_colours: cell_colours(&definition, mode),
            supported: definition.supports_mode(&mode.name),
        })
        .collect();
    let lens = definition
        .physical
        .as_ref()
        .and_then(|physical| physical.lens.as_ref());
    Some(LibraryFixture {
        path: entry.path.clone(),
        manufacturer: definition.manufacturer.clone(),
        model: definition.model.clone(),
        kind: definition.type_.clone(),
        moves: modes.iter().any(|mode| mode.moves),
        beam_deg: lens
            .and_then(|lens| Some((lens.degrees_min?, lens.degrees_max?)))
            .filter(|(min, max)| *max > 0.0 && max >= min),
        modes,
        quality: entry.library.as_ref().map(|l| l.quality.clone()),
    })
}
