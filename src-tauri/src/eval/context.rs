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
//! (`compute_head_offsets` + `head_world_position`); audio/stem/onset/chord
//! loading prefers the app-side DB + audio helpers over raw sqlx where one exists.

use crate::audio::{load_or_decode_audio_shared, stereo_to_mono};
use crate::eval::{ResidentAudio, ResidentContext};
use crate::fixtures::layout::{compute_head_offsets, head_world_position, HeadLayout};
use crate::fixtures::parser::parse_definition;
use crate::models::node_graph::{BeatGrid, Edge, NodeInstance};
use crate::services::tracks::{get_track_beats, TARGET_SAMPLE_RATE};
use once_cell::sync::Lazy;
use serde_json::Value;
use sqlx::{Row, SqlitePool};
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

/// Process-wide cache of per-head GDTF offsets, keyed by `"{fixture_path}|{mode}"`,
/// so fixture definitions are parsed from disk once per venue rather than once per
/// (fixture × annotation).
static OFFSETS_CACHE: Lazy<Mutex<HashMap<String, Vec<HeadLayout>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Drop the cached resident data for a track (called when leaving it).
pub fn clear_track_audio_cache(track_hash: &str) {
    if let Ok(mut c) = AUDIO_CACHE.lock() {
        c.remove(track_hash);
    }
}

/// Read the selection expression a graph scopes to, if any. Two forms:
///   - Static: the expression lives on a Selection-type input (value
///     `{expression, spatialReference}`) surfaced onto a node's `params`, or a
///     flat `tagExpression`/`tag_expr` param.
///   - Arg-wired: the selection is a pattern arg, flowing over an edge from the
///     `pattern_args` node into a node's selection input — the value lives only
///     in `args` (annotation/cue overrides + pattern defaults), keyed by the
///     edge's `from_port` (the arg id).
/// Returns `(expr, seed)` where `seed` is the deterministic per-node hash
/// (matching `LowerCtx::seed` / the legacy executor).
fn graph_selection(
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
) -> Option<(String, u64)> {
    let arg_node_ids: std::collections::HashSet<&str> = nodes
        .iter()
        .filter(|n| n.type_id == "pattern_args")
        .map(|n| n.id.as_str())
        .collect();

    for node in nodes {
        let is_selection_source = matches!(node.type_id.as_str(), "filter_selection" | "select");
        // Prefer an explicit nested selection value `{ expression, spatialReference }`.
        for key in ["selection", "value"] {
            if let Some(expr) = node
                .params
                .get(key)
                .and_then(|v| v.get("expression"))
                .and_then(|v| v.as_str())
            {
                if !expr.trim().is_empty() {
                    return Some((expr.to_string(), seed_for(&node.id)));
                }
            }
        }
        // Flat param forms.
        for key in ["tagExpression", "tag_expr", "expression"] {
            if let Some(expr) = node.params.get(key).and_then(|v| v.as_str()) {
                if !expr.trim().is_empty() {
                    return Some((expr.to_string(), seed_for(&node.id)));
                }
            }
        }
        // Arg-wired form: pattern_args --(arg id)--> this node. Only Selection
        // args carry an `expression` field, so other arg kinds never match.
        for edge in edges {
            if edge.to_node != node.id || !arg_node_ids.contains(edge.from_node.as_str()) {
                continue;
            }
            if let Some(expr) = args
                .get(&edge.from_port)
                .and_then(|v| v.get("expression"))
                .and_then(|v| v.as_str())
            {
                if !expr.trim().is_empty() {
                    return Some((expr.to_string(), seed_for(&node.id)));
                }
            }
        }
        // A selection-source node with no expression still pins the seed.
        if is_selection_source {
            return Some(("all".to_string(), seed_for(&node.id)));
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

/// Deterministic per-node seed: `DefaultHasher(node.id)`, matching `LowerCtx::seed`.
fn seed_for(node_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    node_id.hash(&mut h);
    h.finish()
}

/// Per-head GDTF offsets for a fixture definition (single head at origin if the
/// definition is missing/unparsable).
fn head_offsets(resource_root: &Path, fixture_path: &str, mode_name: &str) -> Vec<HeadLayout> {
    let key = format!("{fixture_path}|{mode_name}");
    if let Ok(cache) = OFFSETS_CACHE.lock() {
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
    }
    let def_path = resource_root.join(fixture_path);
    let offsets = parse_definition(&def_path)
        .ok()
        .map(|def| compute_head_offsets(&def, mode_name))
        .unwrap_or_else(|| {
            vec![HeadLayout {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            }]
        });
    if let Ok(mut cache) = OFFSETS_CACHE.lock() {
        cache.insert(key, offsets.clone());
    }
    offsets
}

/// Resolve the ordered `(primitive_id = "{fixtureUuid}:{head}", world_position)`
/// list a pattern graph covers, for one `(venue, graph)`.
///
/// T-invariant pre-pass: find the graph's selection expression (see
/// [`graph_selection`]); resolve it to venue fixtures via
/// `groups::resolve_selection_expression_with_path` (whole venue when there is no
/// selection node / empty expression — expression `"all"`). Each resolved
/// fixture expands to its *member* heads via `compute_head_offsets` +
/// `head_world_position` (all heads for whole-fixture matches).
pub async fn resolve_primitive_ids(
    project_pool: &SqlitePool,
    venue_id: &str,
    resource_root: &Path,
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
) -> Vec<(String, [f32; 3])> {
    let (expr, seed) =
        graph_selection(nodes, edges, args).unwrap_or_else(|| ("all".to_string(), 0));

    let root_buf = resource_root.to_path_buf();
    let fixtures = crate::services::groups::resolve_selection_expression_with_path(
        &root_buf,
        project_pool,
        venue_id,
        &expr,
        seed,
    )
    .await
    .unwrap_or_default();

    let mut out = Vec::new();
    for resolved in &fixtures {
        let fixture = &resolved.fixture;
        let offsets = head_offsets(resource_root, &fixture.fixture_path, &fixture.mode_name);
        let base = [
            fixture.pos_x as f32,
            fixture.pos_y as f32,
            fixture.pos_z as f32,
        ];
        let rot = [fixture.rot_x, fixture.rot_y, fixture.rot_z];
        let mut push = |i: usize| {
            let pos = head_world_position(base, rot, offsets[i]);
            out.push((format!("{}:{}", fixture.id, i), pos));
        };
        match &resolved.heads {
            // Whole fixture: every head the definition lays out.
            None => (0..offsets.len()).for_each(&mut push),
            // Partial: only member heads (guard against stale indices).
            Some(heads) => heads
                .iter()
                .filter(|&&i| i < offsets.len())
                .for_each(|&i| push(i)),
        }
    }
    out
}

/// Path to the on-disk cache of the final mono-at-analysis-rate audio.
fn eval_mono_cache_path(track_hash: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/com.luma.luma/tracks/cache")
            .join(format!("{track_hash}_eval_mono.pcm")),
    )
}

/// Write mono `ResidentAudio` in the `read_stem_pcm` format (18-byte header +
/// f32 LE samples), so subsequent sessions skip the decode + stereo→mono pass.
fn write_mono_pcm(path: &Path, audio: &ResidentAudio) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(&1u32.to_le_bytes())?; // version
    f.write_all(&audio.sample_rate.to_le_bytes())?; // sample_rate
    f.write_all(&1u16.to_le_bytes())?; // channels = 1 (mono)
    f.write_all(&(audio.samples.len() as u64).to_le_bytes())?; // len
    for s in audio.samples.iter() {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

/// Decode a track's mono resident audio. Three tiers: process-wide in-memory
/// cache (O(1) Arc clone) → on-disk mono PCM (fast read, skips decode/resample/
/// downmix) → full decode (first time only, then written to disk).
fn load_track_audio_cached(file_path: &str, track_hash: &str) -> Option<ResidentAudio> {
    if let Ok(cache) = AUDIO_CACHE.lock() {
        if let Some(hit) = cache.get(track_hash) {
            return Some(hit.clone());
        }
    }
    let mono_path = eval_mono_cache_path(track_hash);

    // Disk tier: a small mono-at-analysis-rate file from a previous session.
    let audio = mono_path
        .as_deref()
        .filter(|p| p.exists())
        .and_then(read_stem_pcm)
        .or_else(|| {
            // Cold: reuse the shared full decode (populated by playback when the
            // track is open — no second decode), downmix once, persist the mono.
            let decoded =
                load_or_decode_audio_shared(Path::new(file_path), track_hash, TARGET_SAMPLE_RATE)
                    .ok()?;
            let audio = ResidentAudio {
                samples: Arc::new(stereo_to_mono(&decoded.samples)),
                sample_rate: decoded.sample_rate,
            };
            if let Some(p) = &mono_path {
                if let Err(e) = write_mono_pcm(p, &audio) {
                    log::warn!("[ctx] failed to write mono audio cache: {e}");
                }
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

/// Read a cached stem `.pcm` (version u32, sample_rate u32, channels u16, len
/// u64, then `len` f32 LE — the `audio::cache` format) into mono ResidentAudio.
fn read_stem_pcm(path: &Path) -> Option<ResidentAudio> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut hdr = [0u8; 18];
    f.read_exact(&mut hdr).ok()?;
    let sample_rate = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
    let channels = u16::from_le_bytes(hdr[8..10].try_into().unwrap());
    let len = u64::from_le_bytes(hdr[10..18].try_into().unwrap()) as usize;
    let mut bytes = vec![0u8; len * 4];
    f.read_exact(&mut bytes).ok()?;
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let mono = if channels >= 2 {
        stereo_to_mono(&samples)
    } else {
        samples
    };
    Some(ResidentAudio {
        samples: Arc::new(mono),
        sample_rate,
    })
}

/// Load the stems actually consumed by a `stem_splitter` in this graph (by the
/// `<name>_out` ports wired downstream), from the on-disk PCM cache keyed by hash.
async fn load_needed_stems(
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
    let Ok(home) = std::env::var("HOME") else {
        return out;
    };
    for name in names {
        let path = PathBuf::from(&home)
            .join("Library/Application Support/com.luma.luma/tracks/stems")
            .join(&hash)
            .join("cache")
            .join(format!("{hash}_stem_{name}.pcm"));
        if let Some(audio) = read_stem_pcm(&path) {
            out.insert(name, audio);
        }
    }
    out
}

/// Chord sections `(start, end, root)` from `track_roots.sections_json`. Raw sqlx
/// on the local pool — there is no app-side DB helper that returns the parsed
/// `(start, end, Option<root>)` triples this shape wants.
async fn fetch_chord_sections(
    local_pool: &SqlitePool,
    track_id: &str,
) -> Vec<(f32, f32, Option<u8>)> {
    let row = sqlx::query("SELECT sections_json FROM track_roots WHERE track_id = ? LIMIT 1")
        .bind(track_id)
        .fetch_optional(local_pool)
        .await
        .ok()
        .flatten();
    let Some(json) = row.and_then(|r| r.try_get::<String, _>(0).ok()) else {
        return Vec::new();
    };
    let Ok(arr) = serde_json::from_str::<Vec<Value>>(&json) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|s| {
            Some((
                s["start"].as_f64()? as f32,
                s["end"].as_f64()? as f32,
                s["root"].as_u64().map(|r| r as u8),
            ))
        })
        .collect()
}

/// Build the full [`ResidentContext`] for a `(track, venue, graph)`, plus the
/// ordered `primitive_ids`. Each subsystem (audio / stems / onsets / chords) is
/// loaded only when the graph actually consumes it.
///
/// `local_pool` is the local DB (`luma.db`: tracks, beats, onsets, roots, stems
/// hash); `project_pool` is the project DB (fixtures / groups for the venue).
#[allow(clippy::too_many_arguments)]
pub async fn build_resident_context(
    local_pool: &SqlitePool,
    project_pool: &SqlitePool,
    resource_root: &Path,
    track_id: &str,
    venue_id: &str,
    nodes: &[NodeInstance],
    edges: &[Edge],
    args: &HashMap<String, serde_json::Value>,
    span: (f32, f32),
    beat_grid_override: Option<BeatGrid>,
) -> (ResidentContext, Vec<String>) {
    // Selection pre-pass → ordered primitive ids + positions.
    let resolved =
        resolve_primitive_ids(project_pool, venue_id, resource_root, nodes, edges, args).await;
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
            Ok(info) => load_track_audio_cached(&info.file_path, &info.track_hash),
            Err(_) => None,
        }
    } else {
        None
    };

    // Stems: only when the graph splits stems.
    let stems = load_needed_stems(local_pool, nodes, edges, track_id).await;

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
        fetch_chord_sections(local_pool, track_id).await
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
