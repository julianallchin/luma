//! Shared physical-head resolution and resident track audio.
//! Score graphs load analyzed track sources through `track_features`.
//!
//! [`resolve_primitive_ids`] resolves a venue's whole rig to its heads and emits
//! `("{fixtureUuid}:{head}", world_position)` in a stable order.
//!
//! Geometry mirrors the legacy mapping exactly via `fixtures::layout`
//! (`head_geometry` + `fixture_kinematics::rig_position`).

use crate::audio::{
    load_or_decode_audio_shared, read_pcm_file, stereo_to_mono, write_pcm_file, SAMPLE_RATE,
};
use crate::eval::ResidentAudio;
use crate::fixtures::layout::{fixture_mount, head_geometry};
use crate::fixtures::parser::parse_definition;
use crate::models::selection::Selection;
use crate::storage::StorageRoot;
use fixture_kinematics::{rig_position, FixtureGeometry};
use once_cell::sync::Lazy;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Process-wide cache of decoded mono track audio, keyed by `track_hash`. The
/// context builder runs once per annotation, so without this a track with N
/// audio-reactive annotations would re-decode the whole file N times (the
/// minute-long composite). Samples are `Arc`-shared, so a hit is an O(1) clone.
/// Capped at a few tracks (decoded audio is tens of MB each).
static AUDIO_CACHE: Lazy<Mutex<HashMap<String, ResidentAudio>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
const AUDIO_CACHE_MAX: usize = 8;

/// Process-wide cache of per-fixture cell geometry, keyed by
/// definition path and mode, so fixture definitions are parsed from disk once
/// per venue rather than once per (fixture × annotation).
static OFFSETS_CACHE: Lazy<Mutex<HashMap<(PathBuf, String), FixtureGeometry>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Per-head offsets for a fixture definition. Missing or invalid definitions
/// are errors: inventing a single head would hide missing pixel geometry.
///
/// Derived from the QLC+ `Physical` block — a housing size and a pixel grid.
/// QLC+ carries no pivot or aperture geometry, so these are positions on the
/// housing face and nothing more.
fn head_offsets(
    resource_root: &Path,
    fixture_path: &str,
    mode_name: &str,
) -> Result<FixtureGeometry, String> {
    let def_path = resource_root.join(fixture_path);
    let key = (def_path.clone(), mode_name.to_owned());
    if let Ok(cache) = OFFSETS_CACHE.lock() {
        if let Some(hit) = cache.get(&key) {
            return Ok(hit.clone());
        }
    }
    let definition = parse_definition(&def_path)
        .map_err(|error| format!("Failed to load fixture definition {fixture_path}: {error}"))?;
    if !definition.modes.iter().any(|mode| mode.name == mode_name) {
        return Err(format!(
            "Fixture definition {fixture_path} has no mode {mode_name}"
        ));
    }
    let offsets = head_geometry(&definition, mode_name);
    if let Ok(mut cache) = OFFSETS_CACHE.lock() {
        cache.insert(key, offsets.clone());
    }
    Ok(offsets)
}

/// Resolve the ordered `(primitive_id = "{fixtureUuid}:{head}", world_position)`
/// list of a venue's whole rig. Each fixture expands to all its heads via
/// `head_geometry` + `fixture_kinematics::rig_position`.
pub async fn resolve_primitive_ids(
    project_pool: &SqlitePool,
    venue_id: &str,
    resource_root: &Path,
) -> Vec<(String, [f32; 3])> {
    // The pool-owning form is the only one that can take the write transaction
    // the old-schema conversion needs; the `_with_access` form is handed a read
    // transaction and an unconverted venue simply resolves to nothing.
    if let Err(e) = crate::venue_graph::ensure_migrated(project_pool, venue_id, resource_root).await
    {
        log::warn!("[ctx] venue {venue_id} could not be converted to a graph: {e}");
    }
    let Ok(mut access) = crate::database::local::venue_access::VenueAccess::<
        crate::database::local::venue_access::Read,
    >::read(
        project_pool,
        crate::database::local::venue_access::VenueResource::Venue(venue_id),
    )
    .await
    else {
        return Vec::new();
    };
    resolve_primitive_ids_with_access(&mut access, resource_root).await
}

/// Resolve the whole rig inside an already-authorized venue snapshot. Agent
/// bindings use this form so fixtures, groups, and positions cannot observe
/// different principals or database revisions within one manifest build.
pub async fn resolve_primitive_ids_with_access(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    resource_root: &Path,
) -> Vec<(String, [f32; 3])> {
    match resolve_selection_primitives_with_access(access, resource_root, &Selection::all(), 0)
        .await
    {
        Ok(primitives) => primitives,
        Err(error) => {
            log::warn!("[ctx] selection could not be resolved: {error}");
            Vec::new()
        }
    }
}

/// Resolve an explicit selection into physical heads, preserving errors for
/// callers that must explain a failed preview rather than return an empty rig.
pub(crate) async fn resolve_selection_primitives_with_access(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    resource_root: &Path,
    selection: &Selection,
    seed: u64,
) -> Result<Vec<(String, [f32; 3])>, String> {
    // One solve for the whole pre-pass: a primitive's position is where its
    // fixture hangs, and every selected fixture is a node of the same venue.
    let venue = crate::venue_graph::resolved(access, resource_root).await?;

    let root_buf = resource_root.to_path_buf();
    let fixtures = crate::services::groups::resolve_selection_expression_with_path(
        &root_buf, access, selection, seed,
    )
    .await?;

    let mut out = Vec::new();
    for resolved in &fixtures {
        let fixture = &resolved.fixture;
        // A fixture the solve cannot reach emits no primitives at all rather
        // than a stack of them at the origin.
        let Some(pose) = venue.pose(&fixture.id) else {
            continue;
        };
        let geom = head_offsets(resource_root, &fixture.fixture_path, &fixture.mode_name)?;
        let mount = fixture_mount(pose);
        let mut push = |i: usize| {
            let pos = rig_position(&geom, &mount, i).to_array();
            out.push((format!("{}:{}", fixture.id, i), pos));
        };
        match &resolved.heads {
            // Whole fixture: every head the definition lays out.
            None => (0..geom.cell_count()).for_each(&mut push),
            // Partial: only member heads (guard against stale indices).
            Some(heads) => heads
                .iter()
                .filter(|&&i| i < geom.cell_count())
                .for_each(|&i| push(i)),
        }
    }
    Ok(out)
}

/// Decode a track's mono resident audio. Three tiers: process-wide in-memory
/// cache (O(1) Arc clone) → on-disk mono PCM (fast read, skips decode/downmix)
/// → the shared stereo decode cache (then written to disk as mono).
pub(crate) fn load_track_audio_cached(
    storage: &StorageRoot,
    file_path: &str,
    track_hash: &str,
) -> Result<ResidentAudio, String> {
    if let Ok(cache) = AUDIO_CACHE.lock() {
        if let Some(hit) = cache.get(track_hash) {
            return Ok(hit.clone());
        }
    }
    let mono_path = storage.eval_mono_pcm_path(track_hash);

    // Disk tier: a small mono file from a previous session.
    let audio = match read_mono_pcm(&mono_path) {
        Some(audio) => audio,
        None => {
            let decoded = load_or_decode_audio_shared(Path::new(file_path), track_hash)
                .map_err(|error| format!("track audio unavailable at {file_path}: {error}"))?;
            let audio = Arc::new(stereo_to_mono(&decoded));
            if let Err(e) = write_pcm_file(&mono_path, &audio, SAMPLE_RATE, 1) {
                log::warn!("[ctx] failed to write mono audio cache: {e}");
            }
            audio
        }
    };

    if let Ok(mut cache) = AUDIO_CACHE.lock() {
        // Evict one entry when at capacity (LRU-ish; audio buffers are large).
        if cache.len() >= AUDIO_CACHE_MAX {
            if let Some(k) = cache.keys().next().cloned() {
                cache.remove(&k);
            }
        }
        cache.insert(track_hash.to_string(), audio.clone());
    }
    Ok(audio)
}

/// Eval-side adapter over the shared PCM reader. A missing or unreadable file,
/// or anything but mono at [`SAMPLE_RATE`], is a cache miss, not an error.
fn read_mono_pcm(path: &Path) -> Option<ResidentAudio> {
    if !path.exists() {
        return None;
    }
    let pcm = match read_pcm_file(path) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("[ctx] ignoring unreadable pcm cache: {e}");
            return None;
        }
    };
    ((pcm.sample_rate, pcm.channels) == (SAMPLE_RATE, 1)).then(|| Arc::new(pcm.samples))
}

#[cfg(test)]
mod head_geometry_tests {
    use super::*;

    #[test]
    fn missing_definition_is_not_a_single_invented_head_or_a_cache_hit_from_another_root() {
        let valid = tempfile::tempdir().unwrap();
        let missing = tempfile::tempdir().unwrap();
        std::fs::write(
            valid.path().join("fixture.qxf"),
            r#"<FixtureDefinition>
            <Manufacturer>Test</Manufacturer><Model>Head</Model><Type>LED</Type>
            <Mode Name="one"/>
        </FixtureDefinition>"#,
        )
        .unwrap();
        assert!(head_offsets(valid.path(), "fixture.qxf", "one").is_ok());
        assert!(head_offsets(valid.path(), "fixture.qxf", "unknown")
            .unwrap_err()
            .contains("no mode unknown"));
        let error = head_offsets(missing.path(), "fixture.qxf", "one").unwrap_err();
        assert!(error.contains("fixture.qxf"));
        assert!(error.contains("Failed to load fixture definition"));
        std::fs::write(missing.path().join("fixture.qxf"), "not XML").unwrap();
        assert!(head_offsets(missing.path(), "fixture.qxf", "one").is_err());
    }
}
