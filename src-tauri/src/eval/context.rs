//! Reusable builder for the eval engine's per-`(track, venue, graph)` inputs.
//!
//! Assembles a [`crate::eval::ResidentContext`] plus the ordered `primitive_ids`
//! the pattern covers. This is the single way the app (and the golden harness)
//! turns a `(track, venue, pattern-graph)` into eval inputs — it replaces the
//! ad-hoc assembly that used to live in `bin/run_goldens.rs`.
//!
//! Two entry points:
//!   - [`resolve_primitive_ids`] — the t-invariant selection pre-pass: resolve the
//!     graph's selection expression to venue fixtures, expand to heads, and emit
//!     `("{fixtureUuid}:{head}", world_position)` in a stable order.
//!   - [`build_resident_context`] — the full context: positions (from the above),
//!     beat grid, resident audio, stems, drum onsets, and chord sections — each
//!     loaded only when the graph actually consumes it.
//!
//! Geometry mirrors the legacy mapping exactly via `fixtures::layout`
//! (`head_geometry` + `fixture_kinematics::rig_position`); audio/stem/onset/chord
//! loading prefers the app-side DB + audio helpers over raw sqlx where one exists.

use crate::audio::{load_or_decode_audio_shared, read_pcm_file, stereo_to_mono, write_pcm_file};
use crate::eval::{ResidentAudio, ResidentContext};
use crate::fixtures::layout::{fixture_mount, head_geometry};
use crate::fixtures::parser::parse_definition;
use crate::models::node_graph::{BeatGrid, Edge, NodeInstance};
use crate::models::selection::Selection;
use crate::services::tracks::{get_track_beats, TARGET_SAMPLE_RATE};
use crate::storage::StorageRoot;
use fixture_kinematics::{rig_position, FixtureGeometry};
use once_cell::sync::Lazy;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::Path;
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
/// `"{fixture_path}|{mode}"`, so fixture definitions are parsed from disk once
/// per venue rather than once per (fixture × annotation).
static OFFSETS_CACHE: Lazy<Mutex<HashMap<String, FixtureGeometry>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Drop the cached resident data for a track (called when leaving it).
pub fn clear_track_audio_cache(track_hash: &str) {
    if let Ok(mut c) = AUDIO_CACHE.lock() {
        c.remove(track_hash);
    }
}

/// Read the [`Selection`] a graph scopes to, if any. Two forms:
///   - Static: the value lives on a Selection-type input surfaced onto a node's
///     `params`, or as a flat `tagExpression`/`tag_expr` param (expression only —
///     a flat param has no subset).
///   - Arg-wired: the selection is a pattern arg, flowing over an edge from the
///     `pattern_args` node into a node's selection input — the value lives only
///     in `args` (annotation/cue overrides + pattern defaults), keyed by the
///     edge's `from_port` (the arg id).
///
/// Returns `(selection, seed)` where `seed` is the deterministic per-node hash
/// (matching `LowerCtx::seed` / the legacy executor) — see the determinism
/// contract on `groups::resolve_selection_expression_with_path`.
fn graph_selection(
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
    instance: Option<&str>,
) -> Option<(Selection, u64)> {
    let arg_node_ids: std::collections::HashSet<&str> = nodes
        .iter()
        .filter(|n| n.type_id == "pattern_args")
        .map(|n| n.id.as_str())
        .collect();

    for node in nodes {
        let is_selection_source = matches!(node.type_id.as_str(), "filter_selection" | "select");
        // Prefer an explicit nested selection value.
        for key in ["selection", "value"] {
            if let Some(selection) = node.params.get(key).and_then(Selection::from_value) {
                if !selection.expression.trim().is_empty() {
                    return Some((selection, seed_for(instance, &node.id)));
                }
            }
        }
        // Flat param forms.
        for key in ["tagExpression", "tag_expr", "expression"] {
            if let Some(expr) = node.params.get(key).and_then(|v| v.as_str()) {
                if !expr.trim().is_empty() {
                    return Some((Selection::new(expr), seed_for(instance, &node.id)));
                }
            }
        }
        // Arg-wired form: pattern_args --(arg id)--> this node. Only Selection
        // args carry an `expression` field, so other arg kinds never match.
        for edge in edges {
            if edge.to_node != node.id || !arg_node_ids.contains(edge.from_node.as_str()) {
                continue;
            }
            if let Some(selection) = args.get(&edge.from_port).and_then(Selection::from_value) {
                if !selection.expression.trim().is_empty() {
                    return Some((selection, seed_for(instance, &node.id)));
                }
            }
        }
        // A selection-source node with no expression still pins the seed.
        if is_selection_source {
            return Some((Selection::all(), seed_for(instance, &node.id)));
        }
    }
    None
}

/// Whether any node requires the host to decode the track audio buffer. Mirrors
/// `node_graph::context::needs_audio_context` (that module is private to
/// `node_graph`, so the predicate is duplicated here rather than re-exported).
fn needs_audio_context(nodes: &[NodeInstance]) -> bool {
    nodes.iter().any(|n| {
        matches!(
            n.type_id.as_str(),
            "audio_input"
                | "stem_splitter"
                | "harmony_analysis"
                | "lowpass_filter"
                | "highpass_filter"
        )
    })
}

/// Deterministic seed for one node's selection draw.
///
/// The node id alone is `DefaultHasher(node.id)`, matching `LowerCtx::seed`.
/// An [`instance`](resolve_primitive_ids) — the clip or cue this occurrence of
/// the pattern belongs to — is mixed in after it, so the same pattern placed
/// twice draws two different halves of a group while either clip on its own
/// draws the same one on every run. Omitting the instance leaves the seed
/// exactly what it was before instances existed.
pub(crate) fn seed_for(instance: Option<&str>, node_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    node_id.hash(&mut h);
    if let Some(instance) = instance {
        instance.hash(&mut h);
    }
    h.finish()
}

/// Per-head offsets for a fixture definition (single head at origin if the
/// definition is missing/unparsable).
///
/// Derived from the QLC+ `Physical` block — a housing size and a pixel grid.
/// QLC+ carries no pivot or aperture geometry, so these are positions on the
/// housing face and nothing more.
fn head_offsets(resource_root: &Path, fixture_path: &str, mode_name: &str) -> FixtureGeometry {
    let key = format!("{fixture_path}|{mode_name}");
    if let Ok(cache) = OFFSETS_CACHE.lock() {
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
    }
    let def_path = resource_root.join(fixture_path);
    let offsets = parse_definition(&def_path)
        .ok()
        .map(|def| head_geometry(&def, mode_name))
        .unwrap_or_else(|| FixtureGeometry::unauthored(Vec::new()));
    if let Ok(mut cache) = OFFSETS_CACHE.lock() {
        cache.insert(key, offsets.clone());
    }
    offsets
}

/// Resolve the ordered `(primitive_id = "{fixtureUuid}:{head}", world_position)`
/// list a pattern graph covers, for one `(venue, graph)`.
///
/// T-invariant pre-pass: find the graph's selection (see [`graph_selection`]);
/// resolve it to venue fixtures via
/// `groups::resolve_selection_expression_with_path` (whole venue when there is no
/// selection node / empty expression — expression `"all"`). Each resolved
/// fixture expands to its *member* heads via `head_geometry` +
/// `fixture_kinematics::rig_position` (all heads for whole-fixture matches).
///
/// `instance` identifies the *occurrence* of the pattern — the clip or cue id —
/// and is what makes the same pattern draw a different half of a group each time
/// it is placed. `None` where there is no occurrence: a pattern's own hover
/// preview, or the venue manifest's whole-rig probe. Preview and render must
/// pass the same instance or they disagree about which lights the clip owns.
pub async fn resolve_primitive_ids(
    project_pool: &SqlitePool,
    venue_id: &str,
    resource_root: &Path,
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
    instance: Option<&str>,
) -> Vec<(String, [f32; 3])> {
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
    resolve_primitive_ids_with_access(&mut access, resource_root, nodes, edges, args, instance)
        .await
}

/// Resolve primitives inside an already-authorized venue snapshot. Agent
/// bindings use this form so fixtures, groups, and positions cannot observe
/// different principals or database revisions within one manifest build.
pub async fn resolve_primitive_ids_with_access(
    access: &mut impl crate::database::local::venue_access::AuthorizedVenue,
    resource_root: &Path,
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
    instance: Option<&str>,
) -> Vec<(String, [f32; 3])> {
    let (selection, seed) =
        graph_selection(nodes, edges, args, instance).unwrap_or_else(|| (Selection::all(), 0));

    let root_buf = resource_root.to_path_buf();
    let fixtures = crate::services::groups::resolve_selection_expression_with_path(
        &root_buf, access, &selection, seed,
    )
    .await
    .unwrap_or_default();

    let mut out = Vec::new();
    for resolved in &fixtures {
        let fixture = &resolved.fixture;
        let geom = head_offsets(resource_root, &fixture.fixture_path, &fixture.mode_name);
        let mount = fixture_mount(
            [fixture.pos_x, fixture.pos_y, fixture.pos_z],
            [fixture.rot_x, fixture.rot_y, fixture.rot_z],
        );
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
    out
}

/// Decode a track's mono resident audio. Three tiers: process-wide in-memory
/// cache (O(1) Arc clone) → on-disk mono PCM (fast read, skips decode/resample/
/// downmix) → full decode (first time only, then written to disk).
fn load_track_audio_cached(
    storage: &StorageRoot,
    file_path: &str,
    track_hash: &str,
) -> Option<ResidentAudio> {
    if let Ok(cache) = AUDIO_CACHE.lock() {
        if let Some(hit) = cache.get(track_hash) {
            return Some(hit.clone());
        }
    }
    let mono_path = storage.eval_mono_pcm_path(track_hash);

    // Disk tier: a small mono-at-analysis-rate file from a previous session.
    let audio = read_mono_pcm(&mono_path).or_else(|| {
        // Cold: reuse the shared full decode (populated by playback when the
        // track is open — no second decode), downmix once, persist the mono.
        let decoded =
            load_or_decode_audio_shared(Path::new(file_path), track_hash, TARGET_SAMPLE_RATE)
                .ok()?;
        let audio = ResidentAudio {
            samples: Arc::new(stereo_to_mono(&decoded.samples)),
            sample_rate: decoded.sample_rate,
        };
        if let Err(e) = write_pcm_file(&mono_path, &audio.samples, audio.sample_rate, 1) {
            log::warn!("[ctx] failed to write mono audio cache: {e}");
        }
        Some(audio)
    })?;

    if let Ok(mut cache) = AUDIO_CACHE.lock() {
        // Evict one entry when at capacity (LRU-ish; audio buffers are large).
        if cache.len() >= AUDIO_CACHE_MAX {
            if let Some(k) = cache.keys().next().cloned() {
                cache.remove(&k);
            }
        }
        cache.insert(track_hash.to_string(), audio.clone());
    }
    Some(audio)
}

/// Eval-side adapter over the shared PCM reader: the engine's `ResidentAudio` is
/// always mono, so a stereo cache file is downmixed on the way in. A missing or
/// unreadable file is a cache miss, not an error.
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
    let mono = if pcm.channels >= 2 {
        stereo_to_mono(&pcm.samples)
    } else {
        pcm.samples
    };
    Some(ResidentAudio {
        samples: Arc::new(mono),
        sample_rate: pcm.sample_rate,
    })
}

/// Load the stems actually consumed by a `stem_splitter` in this graph (by the
/// `<name>_out` ports wired downstream), from the on-disk PCM cache keyed by hash.
async fn load_needed_stems(
    storage: &StorageRoot,
    local_pool: &SqlitePool,
    nodes: &[NodeInstance],
    edges: &[Edge],
    track_id: &str,
) -> HashMap<String, ResidentAudio> {
    let mut out = HashMap::new();
    let splitter_ids: Vec<&str> = nodes
        .iter()
        .filter(|n| n.type_id == "stem_splitter")
        .map(|n| n.id.as_str())
        .collect();
    if splitter_ids.is_empty() {
        return out;
    }
    let mut names: std::collections::BTreeSet<String> = Default::default();
    for e in edges {
        if splitter_ids.contains(&e.from_node.as_str()) {
            if let Some(stem) = e.from_port.strip_suffix("_out") {
                names.insert(stem.to_string());
            }
        }
    }
    let hash =
        match crate::database::local::tracks::get_track_path_and_hash(local_pool, track_id).await {
            Ok(info) => info.track_hash,
            Err(_) => return out,
        };
    for name in names {
        let path = storage.stem_pcm_path(&hash, &name);
        match read_mono_pcm(&path) {
            Some(audio) => {
                out.insert(name, audio);
            }
            // Not fatal: a graph that splits stems must still evaluate (the
            // missing stem reads as silence). Previously silent — log it, since
            // "my stem pattern does nothing" had no diagnostic at all.
            None => log::warn!(
                "[ctx] stem '{name}' unavailable for track {track_id} at {} — \
                 evaluating as silence",
                path.display()
            ),
        }
    }
    out
}

/// Build the full [`ResidentContext`] for a `(track, venue, graph)`, plus the
/// ordered `primitive_ids`. Each subsystem (audio / stems / onsets / chords) is
/// loaded only when the graph actually consumes it.
///
/// `local_pool` is the local DB (`luma.db`: tracks, beats, onsets, roots, stems
/// hash); `project_pool` is the project DB (fixtures / groups for the venue);
/// `storage` resolves the on-disk audio/stem caches. `instance` is the clip or
/// cue this evaluation belongs to — see [`resolve_primitive_ids`].
#[allow(clippy::too_many_arguments)]
pub async fn build_resident_context(
    local_pool: &SqlitePool,
    project_pool: &SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    track_id: &str,
    venue_id: &str,
    instance: Option<&str>,
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
    span: (f32, f32),
    beat_grid_override: Option<BeatGrid>,
) -> (ResidentContext, Vec<String>) {
    // Selection pre-pass → ordered primitive ids + positions.
    let resolved = resolve_primitive_ids(
        project_pool,
        venue_id,
        resource_root,
        nodes,
        edges,
        args,
        instance,
    )
    .await;
    let mut primitive_ids = Vec::with_capacity(resolved.len());
    let mut positions = Vec::with_capacity(resolved.len());
    for (id, pos) in resolved {
        primitive_ids.push(id);
        positions.push(pos);
    }

    // Beat grid: override wins, else fetch.
    let beat_grid = match beat_grid_override {
        Some(g) => Some(g),
        None => get_track_beats(local_pool, track_id).await.ok().flatten(),
    };

    // Resident mono audio: only when an audio-reactive node needs it. Decoded
    // once per track and cached (Arc-shared) — the per-annotation builder must not
    // re-decode the whole file each call.
    let audio = if needs_audio_context(nodes) {
        match crate::database::local::tracks::get_track_path_and_hash(local_pool, track_id).await {
            Ok(info) => load_track_audio_cached(storage, &info.file_path, &info.track_hash),
            Err(_) => None,
        }
    } else {
        None
    };

    // Stems: only when the graph splits stems.
    let stems = load_needed_stems(storage, local_pool, nodes, edges, track_id).await;

    // Drum onsets: only when the graph fires on drum events.
    let drum_onsets = if nodes.iter().any(|n| n.type_id == "drum_events") {
        crate::database::local::tracks::get_track_drum_onsets(local_pool, track_id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    } else {
        HashMap::new()
    };

    // Chord sections: only when the graph does harmony analysis.
    let chord_sections = if nodes.iter().any(|n| n.type_id == "harmony_analysis") {
        crate::database::local::tracks::get_track_chord_sections(local_pool, track_id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.start_s, s.end_s, s.root_pitch_class))
            .collect()
    } else {
        Vec::new()
    };

    let ctx = ResidentContext {
        span,
        positions,
        beat_grid,
        audio,
        stems,
        drum_onsets,
        chord_sections,
        ..Default::default()
    };
    (ctx, primitive_ids)
}
