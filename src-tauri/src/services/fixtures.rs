//! Business logic for fixture operations.
//!
//! Database layer handles CRUD only. File/resource access, ArtNet refresh, and
//! in-memory fixture index live here.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{AppHandle, Manager};

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::venue_access::AuthorizedVenue;
use crate::fixtures::parser::{self, FixtureIndex};
use crate::models::fixtures::{
    FixtureDefinition, FixtureEntry, FixtureNode, FixtureNodeType, PatchedFixture,
};
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
    let index = parser::build_index(root).map_err(|e| e.to_string())?;
    let count = index.entries.len();
    *state.0.lock().unwrap() = Some(index);
    Ok(count)
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
pub fn matches(entry: &FixtureEntry, query: &str) -> bool {
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
/// A stable sort, so entries that score alike keep the index's own order.
#[must_use]
pub fn best_first<'a>(entries: &'a [FixtureEntry], query: &str) -> Vec<&'a FixtureEntry> {
    let mut found: Vec<&FixtureEntry> = entries
        .iter()
        .filter(|entry| matches(entry, query))
        .collect();
    if query.split_whitespace().next().is_some() {
        found.sort_by_key(|entry| distance(entry, query));
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
    /// Relative to the fixtures root — the `fixture` argument of `distribute`.
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
}

/// The fixtures an agent can name, `query` matched by [`matches`].
///
/// Reads the same index the patch page searches — [`parser::build_index`] is a
/// directory walk, not a parse — and then parses only the page it returns, so
/// naming a fixture costs one file read per row rather than one per library.
/// A definition that will not parse is dropped rather than failing the call:
/// the answer to "what can I use" must not be an error because of a file
/// nobody asked about.
///
/// # Errors
/// Fails only if the fixtures root cannot be walked.
pub fn library(root: &Path, query: &str, limit: usize) -> Result<Vec<LibraryFixture>, String> {
    let index = parser::build_index(root).map_err(|e| e.to_string())?;
    Ok(best_first(&index.entries, query)
        .into_iter()
        .filter_map(|entry| resolve_entry(root, entry))
        .take(limit)
        .collect())
}

/// One library entry with its definition read, or `None` if it will not parse.
fn resolve_entry(root: &Path, entry: &FixtureEntry) -> Option<LibraryFixture> {
    let definition = parser::parse_definition(&root.join(&entry.path)).ok()?;
    let modes: Vec<LibraryMode> = definition
        .modes
        .iter()
        .map(|mode| LibraryMode {
            name: mode.name.clone(),
            channels: mode.channels.len(),
            moves: group_derivation::aims(&definition, mode),
            role: group_derivation::FixtureRole::of(&definition, mode)
                .as_str()
                .to_string(),
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
    })
}

/// Get fixture definition from a path relative to the fixtures root.
///
/// The caller is responsible for rejecting a `path` that escapes `root`.
pub fn get_fixture_definition(root: &Path, path: &Path) -> Result<FixtureDefinition, String> {
    parser::parse_definition(&root.join(path)).map_err(|e| e.to_string())
}

/// Get all patched fixtures for a venue
pub async fn get_patched_fixtures(
    access: &mut impl AuthorizedVenue,
) -> Result<Vec<PatchedFixture>, String> {
    fixtures_db::get_patched_fixtures(access).await
}

/// Get patch hierarchy for a venue
pub async fn get_patch_hierarchy(
    root: &Path,
    access: &mut impl AuthorizedVenue,
) -> Result<Vec<FixtureNode>, String> {
    let fixtures = fixtures_db::get_patched_fixtures(access).await?;

    let mut hierarchy = Vec::new();
    for fixture in fixtures {
        let def_path = root.join(&fixture.fixture_path);
        let mut children = Vec::new();

        if let Ok(def) = parser::parse_definition(&def_path) {
            if let Some(mode) = def.modes.iter().find(|m| m.name == fixture.mode_name) {
                if !mode.heads.is_empty() {
                    for (i, _head) in mode.heads.iter().enumerate() {
                        children.push(FixtureNode {
                            id: format!("{}:{}", fixture.id, i),
                            label: format!("Head {}", i + 1),
                            type_: FixtureNodeType::Head,
                            children: vec![],
                        });
                    }
                }
            }
        }

        hierarchy.push(FixtureNode {
            id: fixture.id.clone(),
            label: fixture
                .label
                .clone()
                .unwrap_or_else(|| format!("{} {}", fixture.manufacturer, fixture.model)),
            type_: FixtureNodeType::Fixture,
            children,
        });
    }

    Ok(hierarchy)
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

pub fn resolve_fixtures_root(app: &AppHandle) -> Result<PathBuf, String> {
    resolve_fixtures_root_from(app.path().resource_dir().ok().as_deref())
}

/// [`resolve_fixtures_root`] without an `AppHandle`. Headless binaries pass
/// `None` (or an explicit resource dir) and get the identical search order.
pub fn resolve_fixtures_root_from(resource_dir: Option<&Path>) -> Result<PathBuf, String> {
    // In debug builds, prefer the source directory so newly added fixture files
    // are picked up immediately without needing a full Tauri resource re-bundle.
    #[cfg(debug_assertions)]
    {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let dev_path = cwd.join("../resources/fixtures/2511260420");
        if dev_path.exists() {
            return Ok(dev_path);
        }
    }

    if let Some(resource_dir) = resource_dir {
        // Bundled app: "../resources/fixtures" maps to "_up_/resources/fixtures"
        let bundled = resource_dir.join("_up_/resources/fixtures/2511260420");
        if bundled.exists() {
            return Ok(bundled);
        }
    }

    // Dev fallback: CWD is src-tauri, fixtures are at ../resources/fixtures
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let dev_path = cwd.join("../resources/fixtures/2511260420");
    if dev_path.exists() {
        return Ok(dev_path);
    }

    Err("Could not find fixtures directory".to_string())
}
