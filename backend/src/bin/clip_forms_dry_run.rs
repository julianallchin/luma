//! Dry run of the clip-forms migration (docs/specs/clip-forms.md, Migration).
//!
//! Opens a copy of the library database read-only, converts every clip with
//! `luma_lib::migration::clip_forms`, renders the old and the new clips on the
//! clip's own venue and selection, and writes a report: a markdown summary,
//! one JSON file per clip, and the change set that applies the migration
//! (`proposed_changes.json` and the same set as one SQL transaction). It
//! writes no database and runs no SQL.
//!
//! ```text
//! cargo +1.97.1 run --release --bin clip_forms_dry_run -- \
//!     --db ~/luma-migration/clip-forms/luma-snapshot.db \
//!     --out ~/luma-migration/clip-forms/report
//! ```
use luma_lib::{
    eval::{Arena, CompiledAnnotation, Scene, Scope},
    migration::clip_forms::{convert, repair_draft_inputs, Converted, Host, Proposal},
    models::universe::{PrimitiveState, UniverseState},
    storage::StorageRoot,
};
use luma_patterns::{BeatTimeline, BlendMode, Clip, Drum, Score, Selection};
use serde::Serialize;
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

/// Below this per-channel difference a conversion is exact.
const EXACT: f64 = 2e-6;
/// Below this it is close.
const CLOSE: f64 = 0.02;
/// Evenly spaced samples per clip, besides the event boundaries.
const SAMPLES: usize = 32;
/// Beats on either side of a boundary to sample.
const AROUND: f64 = 0.01;
/// Clip graphs, by definition name, whose clips are removed instead of
/// converted, and why.
const REMOVED: [(&str, &str); 1] = [(
    "Hats · travelling pinpoints",
    "removed by decision: a chase times a hi-hat pulse would need a multiply layer that darkens the clips under it",
)];
/// Room between two clips' z-index in the render check, for their layers.
const STEP: i64 = 16;

struct Arguments {
    database: PathBuf,
    output: PathBuf,
    storage: StorageRoot,
    score: Option<String>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Status {
    Exact,
    Close,
    Different,
    Unmatched,
    /// The old clip cannot be rendered today, so nothing can be compared.
    Unrendered,
    /// The clip is removed.
    Deleted,
}
impl Status {
    const ALL: [Self; 6] = [
        Self::Exact,
        Self::Close,
        Self::Different,
        Self::Unmatched,
        Self::Unrendered,
        Self::Deleted,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Close => "close",
            Self::Different => "different",
            Self::Unmatched => "unmatched",
            Self::Unrendered => "unrendered",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Serialize)]
struct Worst {
    beat: f64,
    head: String,
    channel: &'static str,
    old: f64,
    new: f64,
}

#[derive(Serialize)]
struct ClipReport {
    id: String,
    score_id: String,
    score: String,
    track: String,
    clip: String,
    definition_id: String,
    definition: String,
    look: Option<String>,
    form: Option<String>,
    status: Status,
    max_diff: Option<f64>,
    worst: Option<Worst>,
    reasons: Vec<String>,
    samples: usize,
    samples_over_close: usize,
    /// Beats the start moved: negative is earlier.
    start_moved: f64,
    /// The most light the new clips show before the old start.
    added_light: f64,
    /// Forms of the layers beside the clip, lowest first.
    layers: Vec<String>,
    /// For a clip with a multiply layer over it: the largest difference of
    /// the whole score's light while it plays, which includes the clips the
    /// layer darkens.
    stack_max_diff: Option<f64>,
    old: Json,
    new: Option<Json>,
}

/// Row changes for one table.
#[derive(Default, Serialize)]
struct TableChanges {
    updates: Vec<Json>,
    inserts: Vec<Json>,
    deletes: Vec<Json>,
}

/// The whole change set, by table.
#[derive(Default)]
struct Changes {
    clips: TableChanges,
    /// Full ids of score definitions a clip still names after conversion.
    definitions_in_use: BTreeSet<String>,
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args = Arguments::parse()?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            // Immutable: SQLite neither locks the copy nor adds WAL files.
            SqliteConnectOptions::new()
                .filename(&args.database)
                .read_only(true)
                .immutable(true),
        )
        .await
        .map_err(|e| format!("cannot open {} read-only: {e}", args.database.display()))?;
    let fixtures = luma_lib::headless_host::HostConfig::default().fixtures_root()?;
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    let scores = sqlx::query(
        "SELECT s.id, s.uid, COALESCE(s.name, '') AS name, s.track_id, \
         COALESCE(t.title, '') AS title FROM scores s JOIN tracks t ON t.id = s.track_id \
         WHERE EXISTS (SELECT 1 FROM clips c WHERE c.score_id = s.id) ORDER BY s.id",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?;
    let clips_dir = args.output.join("clips");
    fs::create_dir_all(&clips_dir).map_err(|e| e.to_string())?;
    let mut reports = Vec::new();
    let mut changes = Changes::default();
    for row in scores {
        let score_id: String = row.get("id");
        if args.score.as_ref().is_some_and(|only| *only != score_id) {
            continue;
        }
        let track_id: String = row.get("track_id");
        let title: String = row.get("title");
        let name: String = row.get("name");
        let score_name = if name.is_empty() { title.clone() } else { name };
        eprintln!("{score_name} ({score_id})");
        let run = ScoreRun {
            pool: &pool,
            storage: &args.storage,
            fixtures: &fixtures,
            score_id: &score_id,
            score_uid: row.get("uid"),
            track_id: &track_id,
            now: &now,
        };
        reports.extend(run.run(&score_name, &title, &mut changes).await?);
    }
    for report in &reports {
        let file = clips_dir.join(format!("{}.json", report.id.replace([':', '/'], "_")));
        write(&file, &serde_json::to_string_pretty(report).unwrap())?;
    }
    let set = change_set(&pool, changes, &args, &now).await?;
    write(
        &args.output.join("proposed_changes.json"),
        &serde_json::to_string_pretty(&set).unwrap(),
    )?;
    write(&args.output.join("proposed_changes.sql"), &sql(&set, &now))?;
    let old_rows = args.output.join("proposed_rows.json");
    if old_rows.exists() {
        fs::remove_file(&old_rows).map_err(|e| e.to_string())?;
    }
    let summary = summary(&reports, &set, &args.database);
    write(&args.output.join("summary.md"), &summary)?;
    println!("{}", args.output.join("summary.md").display());
    Ok(())
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A stored clip row as text, for the change set.
struct Stored {
    uid: String,
    graph: String,
    start: f64,
    duration: f64,
    seed: String,
    selection_seed: Option<String>,
    selection_json: String,
    z_index: i64,
    blend_mode: String,
    inputs_json: String,
}

struct Loaded {
    score: Score,
    /// Stored selection JSON per clip key, `subset` included.
    selections: BTreeMap<String, Json>,
    /// Notes from repairing draft form inputs.
    repairs: BTreeMap<String, Vec<String>>,
    unreadable: BTreeMap<String, String>,
    stored: BTreeMap<String, Stored>,
}

struct ScoreRun<'a> {
    pool: &'a SqlitePool,
    storage: &'a StorageRoot,
    fixtures: &'a Path,
    score_id: &'a str,
    score_uid: Option<String>,
    track_id: &'a str,
    now: &'a str,
}

type Compiled = Result<Option<CompiledAnnotation>, String>;

impl ScoreRun<'_> {
    async fn run(
        &self,
        score_name: &str,
        track: &str,
        changes: &mut Changes,
    ) -> Result<Vec<ClipReport>, String> {
        let Loaded {
            mut score,
            selections,
            repairs,
            unreadable,
            stored,
        } = self.load().await?;
        // Clips in paint order: by z-index, then by key, as the scene sorts
        // them. In the render check each clip's z-index is its rank times
        // STEP, so a compiled annotation names its clip and layers fit
        // between clips.
        let mut order: Vec<String> = score.clips.keys().cloned().collect();
        order.sort_by_key(|key| score.clips[key].z_index);
        let rank: BTreeMap<String, i64> = order
            .iter()
            .enumerate()
            .map(|(rank, key)| (key.clone(), rank as i64))
            .collect();
        for (key, clip) in &mut score.clips {
            clip.z_index = rank[key] * STEP;
        }
        let grid = luma_lib::services::tracks::get_track_beats(self.pool, self.track_id)
            .await?
            .ok_or("the track has no beat grid")?;
        let clock = grid.timeline().map_err(|e| e.to_string())?;
        let host = self.host(&clock).await?;
        let old = self.scene(&score).await;

        let mut converted: BTreeMap<String, Result<Proposal, String>> = BTreeMap::new();
        for key in &order {
            let heads = match old.get(&(rank[key] * STEP)) {
                Some(Ok(Some(annotation))) => annotation.plan.primitive_ids.len(),
                _ => 0,
            };
            let host = Host {
                heads,
                ..host.clone()
            };
            let selection = selections.get(key).cloned().unwrap_or_default();
            let name = score
                .definitions
                .get(&score.clips[key].graph)
                .map(|definition| definition.name.as_str());
            let result = match REMOVED.iter().find(|(removed, _)| Some(*removed) == name) {
                Some((_, reason)) => Ok(Proposal::Delete {
                    reason: (*reason).to_owned(),
                }),
                None => convert(&score, key, &selection, &host),
            };
            converted.insert(key.clone(), result);
        }
        // Each clip's layers sit just under and over it.
        let mut next = Score::default();
        for key in &order {
            if let Some(Ok(Proposal::Form(result))) = converted.get(key) {
                let (main, stack) = result.stack();
                for (index, clip) in stack.into_iter().enumerate() {
                    let offset = index as i64 - main as i64;
                    assert!(offset.abs() < STEP / 2, "too many layers");
                    let mut clip = clip.clone();
                    clip.z_index = rank[key] * STEP + offset;
                    let name = if offset == 0 {
                        key.clone()
                    } else {
                        format!("{key}~{index}")
                    };
                    next.clips.insert(name, clip);
                }
            }
        }
        let new = self.scene(&next).await;
        let old_scene: Vec<CompiledAnnotation> = old
            .values()
            .filter_map(|compiled| compiled.as_ref().ok().cloned().flatten())
            .collect();

        let mut reports = Vec::new();
        let layered = converted.values().any(|result| {
            matches!(result, Ok(Proposal::Form(converted)) if !converted.layers.is_empty())
        });
        let mut next_z = 0_i64;
        for key in &order {
            let clip = &score.clips[key];
            let row = &stored[key];
            let definition = score
                .definitions
                .get(&clip.graph)
                .map(|d| d.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| clip.graph.clone());
            let id = format!("{}:{key}", self.score_id);
            let mut report = ClipReport {
                id: id.clone(),
                score_id: self.score_id.into(),
                score: score_name.into(),
                track: track.into(),
                clip: key.clone(),
                definition_id: clip.graph.clone(),
                definition,
                look: None,
                form: None,
                status: Status::Unmatched,
                max_diff: None,
                worst: None,
                reasons: Vec::new(),
                samples: 0,
                samples_over_close: 0,
                start_moved: 0.0,
                added_light: 0.0,
                layers: Vec::new(),
                stack_max_diff: None,
                old: json!({
                    "start": clip.start,
                    "duration": clip.duration,
                    "z_index": row.z_index,
                    "blend_mode": row.blend_mode,
                    "graph": clip.graph,
                    "inputs": clip.inputs,
                    "selection": selections.get(key),
                }),
                new: None,
            };
            let mut z = || {
                let z = next_z;
                next_z += 1;
                if layered {
                    z
                } else {
                    row.z_index
                }
            };
            match &converted[key] {
                Err(reason) => {
                    report.reasons.push(reason.clone());
                    // Unconverted rows keep their place among the others.
                    let z = z();
                    if z != row.z_index {
                        changes.clips.updates.push(json!({
                            "id": id,
                            "uid": row.uid,
                            "set": {"z_index": z, "updated_at": self.now},
                        }));
                    }
                    changes
                        .definitions_in_use
                        .insert(format!("{}:{}", self.score_id, clip.graph));
                }
                Ok(Proposal::Delete { reason }) => {
                    report.status = Status::Deleted;
                    report.reasons.push(reason.clone());
                    changes.clips.deletes.push(json!({
                        "id": id,
                        "uid": row.uid,
                        "reason": reason,
                    }));
                }
                Ok(Proposal::Form(result)) => {
                    let (main, stack) = result.stack();
                    report.look = Some(result.look.clone());
                    report.form = Some(result.clip.graph.clone());
                    report.start_moved = result.clip.start - clip.start;
                    report.layers = stack
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| *index != main)
                        .map(|(_, layer)| format!("{} ({})", layer.graph, layer.blend_mode.name()))
                        .collect();
                    report.reasons = repairs.get(key).cloned().unwrap_or_default();
                    report.reasons.extend(result.notes.iter().cloned());
                    report.new = Some(json!({
                        "start": result.clip.start,
                        "duration": result.clip.duration,
                        "stack": stack.iter().map(|clip| json!({
                            "graph": clip.graph,
                            "blend_mode": clip.blend_mode.name(),
                            "inputs": clip.inputs,
                        })).collect::<Vec<_>>(),
                        "converted": main,
                        "selection": result.clip.selection,
                    }));
                    for (index, new_clip) in stack.iter().enumerate() {
                        let z = z();
                        if index == main {
                            if let Some(update) = self.update(&id, row, new_clip, z) {
                                changes.clips.updates.push(update);
                            }
                        } else {
                            changes
                                .clips
                                .inserts
                                .push(self.insert(&id, row, new_clip, z, index));
                        }
                    }
                    let group: Vec<&Compiled> = (0..stack.len())
                        .map(|index| {
                            new.get(&(rank[key] * STEP + index as i64 - main as i64))
                                .unwrap_or(&Ok(None))
                        })
                        .collect();
                    let own = old.get(&(rank[key] * STEP)).unwrap_or(&Ok(None));
                    let multiplies = result
                        .layers
                        .iter()
                        .any(|layer| layer.above && layer.clip.blend_mode == BlendMode::Multiply);
                    let check = check(
                        &clock,
                        clip,
                        result,
                        own,
                        &group,
                        multiplies.then_some(old_scene.as_slice()),
                    );
                    match check {
                        Err(error) => {
                            report.status = Status::Unrendered;
                            report.reasons.insert(0, error);
                        }
                        Ok(check) => {
                            report.samples = check.samples;
                            report.samples_over_close = check.over_close;
                            report.added_light = check.added;
                            report.stack_max_diff = check.stack;
                            if check.added >= EXACT {
                                report.reasons.push(format!(
                                    "adds light up to {:.3} before the old start",
                                    check.added
                                ));
                            }
                            report.max_diff = Some(check.max);
                            report.status = if check.max < EXACT {
                                Status::Exact
                            } else if check.max < CLOSE {
                                Status::Close
                            } else {
                                Status::Different
                            };
                            if check.bindings_differ {
                                report.reasons.push("writes other channels".into());
                            }
                            if report.status != Status::Exact && report.reasons.is_empty() {
                                report.reasons.push("no known reason".into());
                            }
                            report.worst = check.worst;
                        }
                    }
                }
            }
            reports.push(report);
        }
        for (key, error) in unreadable {
            // The row stays as it is, and so does the graph it names.
            changes.definitions_in_use.extend(
                score
                    .definitions
                    .keys()
                    .map(|definition| format!("{}:{definition}", self.score_id)),
            );
            reports.push(ClipReport {
                id: format!("{}:{key}", self.score_id),
                score_id: self.score_id.into(),
                score: score_name.into(),
                track: track.into(),
                clip: key,
                definition_id: String::new(),
                definition: String::new(),
                look: None,
                form: None,
                status: Status::Unmatched,
                max_diff: None,
                worst: None,
                reasons: vec![format!("unreadable row: {error}")],
                samples: 0,
                samples_over_close: 0,
                start_moved: 0.0,
                added_light: 0.0,
                layers: Vec::new(),
                stack_max_diff: None,
                old: Json::Null,
                new: None,
            });
        }
        Ok(reports)
    }

    /// The new values of a converted clip's row, or `None` when nothing
    /// changes. `uid` stays as it is.
    fn update(&self, id: &str, row: &Stored, clip: &Clip, z: i64) -> Option<Json> {
        let mut set = serde_json::Map::new();
        if clip.graph != row.graph {
            set.insert("graph".into(), json!(clip.graph));
        }
        let inputs = serde_json::to_value(&clip.inputs).unwrap();
        let stored_inputs: Json = serde_json::from_str(&row.inputs_json).unwrap_or_default();
        if inputs != stored_inputs {
            set.insert(
                "inputs_json".into(),
                json!(serde_json::to_string(&clip.inputs).unwrap()),
            );
        }
        if clip.start != row.start || clip.duration != row.duration {
            set.insert("start".into(), json!(clip.start));
            set.insert("duration".into(), json!(clip.duration));
        }
        if z != row.z_index {
            set.insert("z_index".into(), json!(z));
        }
        if clip.blend_mode.name() != row.blend_mode {
            set.insert("blend_mode".into(), json!(clip.blend_mode.name()));
        }
        if let Some(selection) = selection_json(row, &clip.selection) {
            set.insert("selection_json".into(), json!(selection));
        }
        if set.is_empty() {
            return None;
        }
        set.insert("updated_at".into(), json!(self.now));
        Some(json!({"id": id, "uid": row.uid, "set": set}))
    }

    /// A new layer row beside clip `id`.
    fn insert(&self, id: &str, row: &Stored, clip: &Clip, z: i64, index: usize) -> Json {
        let digest = Sha256::digest(format!("clip-forms layer {id} {index}"));
        let bytes: [u8; 16] = digest[..16].try_into().unwrap();
        let key = uuid::Builder::from_random_bytes(bytes).into_uuid();
        json!({
            "id": format!("{}:{key}", self.score_id),
            "uid": self.score_uid.clone().unwrap_or_else(|| row.uid.clone()),
            "score_id": self.score_id,
            "graph": clip.graph,
            "start": clip.start,
            "duration": clip.duration,
            "seed": row.seed,
            "selection_seed": row.selection_seed,
            "selection_json": selection_json(row, &clip.selection)
                .unwrap_or_else(|| row.selection_json.clone()),
            "z_index": z,
            "blend_mode": clip.blend_mode.name(),
            "inputs_json": serde_json::to_string(&clip.inputs).unwrap(),
            "created_at": self.now,
            "updated_at": self.now,
            "layer_of": id,
        })
    }

    /// The score's rows as the app reads them, with draft form inputs
    /// repaired. A row that still does not read is set aside with its error.
    async fn load(&self) -> Result<Loaded, String> {
        let mut loaded = Loaded {
            score: Score::default(),
            selections: BTreeMap::new(),
            repairs: BTreeMap::new(),
            unreadable: BTreeMap::new(),
            stored: BTreeMap::new(),
        };
        let rows = sqlx::query(
            "SELECT id, uid, graph, start, duration, seed, selection_seed, selection_json, \
             z_index, blend_mode, inputs_json FROM clips WHERE score_id = ? ORDER BY id",
        )
        .bind(self.score_id)
        .fetch_all(self.pool)
        .await
        .map_err(|e| e.to_string())?;
        for row in rows {
            let id: String = row.get("id");
            let key = id
                .strip_prefix(self.score_id)
                .and_then(|rest| rest.strip_prefix(':'))
                .unwrap_or(&id)
                .to_owned();
            let selection: Json =
                serde_json::from_str(row.get::<&str, _>("selection_json")).unwrap_or_default();
            let mut inputs: Json =
                serde_json::from_str(row.get::<&str, _>("inputs_json")).unwrap_or_default();
            let repairs = repair_draft_inputs(&mut inputs);
            let clip = (|| -> Result<Clip, String> {
                let seed = |text: &str| text.parse::<u64>().map_err(|e| e.to_string());
                Ok(Clip {
                    graph: row.get("graph"),
                    start: row.get("start"),
                    duration: row.get("duration"),
                    seed: seed(row.get("seed"))?,
                    selection_seed: row
                        .get::<Option<&str>, _>("selection_seed")
                        .map(seed)
                        .transpose()?,
                    selection: Selection::from_value(&selection)
                        .ok_or("the selection has no expression")?,
                    z_index: row.get("z_index"),
                    blend_mode: serde_json::from_value(json!(row.get::<&str, _>("blend_mode")))
                        .map_err(|e| e.to_string())?,
                    inputs: serde_json::from_value(inputs).map_err(|e| e.to_string())?,
                })
            })();
            match clip {
                Ok(clip) => {
                    loaded.score.clips.insert(key.clone(), clip);
                    loaded.selections.insert(key.clone(), selection);
                    loaded.stored.insert(
                        key.clone(),
                        Stored {
                            uid: row.get("uid"),
                            graph: row.get("graph"),
                            start: row.get("start"),
                            duration: row.get("duration"),
                            seed: row.get("seed"),
                            selection_seed: row.get("selection_seed"),
                            selection_json: row.get("selection_json"),
                            z_index: row.get("z_index"),
                            blend_mode: row.get("blend_mode"),
                            inputs_json: row.get("inputs_json"),
                        },
                    );
                    if !repairs.is_empty() {
                        loaded.repairs.insert(key, repairs);
                    }
                }
                Err(error) => {
                    loaded.unreadable.insert(key, error);
                }
            }
        }
        let rows =
            sqlx::query("SELECT id, definition_json FROM score_definitions WHERE score_id = ?")
                .bind(self.score_id)
                .fetch_all(self.pool)
                .await
                .map_err(|e| e.to_string())?;
        for row in rows {
            let id: String = row.get("id");
            let key = id
                .split_once(':')
                .map_or(id.clone(), |(_, key)| key.to_owned());
            let definition = serde_json::from_str(row.get::<&str, _>("definition_json"))
                .map_err(|e| format!("definition {id}: {e}"))?;
            loaded.score.definitions.insert(key, definition);
        }
        // What the row reader repairs on every read.
        luma_patterns::migration::lit_heads_density(&mut loaded.score);
        Ok(loaded)
    }

    /// Onsets and chords in beats.
    async fn host(&self, clock: &BeatTimeline) -> Result<Host, String> {
        let mut host = Host::default();
        let onsets: Option<String> =
            sqlx::query_scalar("SELECT onsets_json FROM track_drum_onsets WHERE track_id = ?")
                .bind(self.track_id)
                .fetch_optional(self.pool)
                .await
                .map_err(|e| e.to_string())?;
        if let Some(json) = onsets {
            let onsets: BTreeMap<String, Vec<f64>> =
                serde_json::from_str(&json).map_err(|e| e.to_string())?;
            for drum in [Drum::Kick, Drum::Snare, Drum::Hihat, Drum::Cymbal] {
                if let Some(times) = onsets.get(drum.name()) {
                    let beats = times
                        .iter()
                        .map(|t| clock.beat_at(*t))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string())?;
                    host.onsets.insert(drum, beats);
                }
            }
        }
        let chords: Option<String> =
            sqlx::query_scalar("SELECT sections_json FROM track_roots WHERE track_id = ?")
                .bind(self.track_id)
                .fetch_optional(self.pool)
                .await
                .map_err(|e| e.to_string())?;
        if let Some(json) = chords {
            let entries: Vec<Json> = serde_json::from_str(&json).map_err(|e| e.to_string())?;
            for entry in entries {
                let (Some(start), Some(end)) = (entry["start"].as_f64(), entry["end"].as_f64())
                else {
                    continue;
                };
                let root = entry
                    .get("root")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| u8::try_from(v).ok());
                host.chords.push((
                    clock.beat_at(start).map_err(|e| e.to_string())?,
                    clock.beat_at(end).map_err(|e| e.to_string())?,
                    root,
                ));
            }
        }
        Ok(host)
    }

    /// Every clip compiled, by z-index. `None` is a clip with no heads on
    /// this venue. A score that fails to compile is compiled one clip at a
    /// time so each failure lands on its own clip.
    async fn scene(&self, score: &Score) -> BTreeMap<i64, Compiled> {
        let mut result = BTreeMap::new();
        match self.compile(score).await {
            Ok(scene) => {
                let mut by_z: BTreeMap<i64, CompiledAnnotation> = scene
                    .annotations
                    .into_iter()
                    .map(|annotation| (annotation.z_index, annotation))
                    .collect();
                for clip in score.clips.values() {
                    result.insert(clip.z_index, Ok(by_z.remove(&clip.z_index)));
                }
            }
            Err(_) => {
                for (key, clip) in &score.clips {
                    let mut single = Score::default();
                    single.definitions = score.definitions.clone();
                    single.clips.insert(key.clone(), clip.clone());
                    let compiled = self
                        .compile(&single)
                        .await
                        .map(|scene| scene.annotations.into_iter().next());
                    result.insert(clip.z_index, compiled);
                }
            }
        }
        result
    }

    async fn compile(&self, score: &Score) -> Result<Scene, String> {
        luma_lib::build_score_scene(
            self.pool,
            self.storage,
            self.fixtures,
            self.score_id,
            Some(score.clone()),
        )
        .await
    }
}

/// A selection to write when the stored one carries a `subset`, which the
/// conversion has used up.
fn selection_json(row: &Stored, selection: &Selection) -> Option<String> {
    let stored: Json = serde_json::from_str(&row.selection_json).unwrap_or_default();
    stored
        .get("subset")
        .map(|_| serde_json::to_string(selection).unwrap())
}

// ---------------------------------------------------------------------------
// Render check

struct Check {
    max: f64,
    worst: Option<Worst>,
    /// Samples inside the old clip, and how many of them differ by at
    /// least [`CLOSE`].
    samples: usize,
    over_close: usize,
    /// The most light the new clips show where the old clip was not.
    added: f64,
    /// The largest difference of the whole score's light, when asked for.
    stack: Option<f64>,
    bindings_differ: bool,
}

/// Render the old clip and the new stack at evenly spaced beats of the old
/// clip and around every boundary, and find the largest difference of
/// emitted light, strobe or aim. Beats the new clips cover before the old
/// start are checked apart: there the old clip is dark, and any light is
/// light the conversion adds. With `scene`, the whole score is also rendered
/// with the old clip and with the new stack in its place.
fn check(
    clock: &BeatTimeline,
    old_clip: &Clip,
    converted: &Converted,
    old: &Compiled,
    group: &[&Compiled],
    scene: Option<&[CompiledAnnotation]>,
) -> Result<Check, String> {
    let old = old
        .as_ref()
        .map_err(|e| format!("old render failed: {e}"))?;
    let group: Vec<CompiledAnnotation> = group
        .iter()
        .map(|compiled| {
            compiled
                .as_ref()
                .map_err(|e| format!("new render failed: {e}"))
                .cloned()
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    let (start, end) = (old_clip.start, old_clip.start + old_clip.duration);
    let mut beats: Vec<f64> = (0..SAMPLES)
        .map(|i| start + old_clip.duration * (i as f64 + 0.5) / SAMPLES as f64)
        .collect();
    for boundary in &converted.boundaries {
        beats.extend([boundary - AROUND, boundary + AROUND]);
    }
    beats.retain(|b| *b > start && *b < end);
    beats.sort_by(f64::total_cmp);
    beats.dedup();
    let inside = beats.len();
    let added = converted.clip.start;
    if added < start {
        beats.extend((0..4).map(|i| added + (start - added) * (i as f64 + 0.5) / 4.0));
    }
    let seconds: Vec<f32> = beats
        .iter()
        .map(|b| clock.seconds_at(*b).map(|s| s as f32))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut arena = Arena::default();
    let mut render = |annotations: Vec<CompiledAnnotation>| {
        Scene::new(annotations).try_render(&seconds, Scope::Composite, &mut arena)
    };
    let before = render(old.iter().cloned().collect())?;
    let after = render(group.clone())?;
    let bound = |o: &luma_lib::eval::OutputBinding| [o.color || o.dimmer, o.strobe, o.position];
    let mut check = Check {
        max: 0.0,
        worst: None,
        samples: inside,
        over_close: 0,
        added: 0.0,
        stack: None,
        bindings_differ: match old {
            Some(old) if !group.is_empty() => {
                let mut written = [false; 3];
                for annotation in &group {
                    for (w, b) in written.iter_mut().zip(bound(&annotation.plan.outputs)) {
                        *w |= b;
                    }
                }
                bound(&old.plan.outputs) != written
            }
            _ => false,
        },
    };
    for (index, ((beat, a), b)) in beats.iter().zip(&before).zip(&after).enumerate() {
        let (sample, worst) = compare(a, b);
        if index >= inside {
            check.added = check.added.max(sample);
            continue;
        }
        if sample >= CLOSE {
            check.over_close += 1;
        }
        if sample > check.max {
            check.max = sample;
            check.worst = worst.map(|(head, channel, old, new)| Worst {
                beat: *beat,
                head,
                channel,
                old,
                new,
            });
        }
    }
    if let (Some(scene), Some(old)) = (scene, old) {
        let others: Vec<CompiledAnnotation> = scene
            .iter()
            .filter(|annotation| annotation.z_index != old.z_index)
            .cloned()
            .collect();
        let whole_before = render(scene.to_vec())?;
        let whole_after = render(others.into_iter().chain(group).collect())?;
        check.stack = Some(
            whole_before
                .iter()
                .zip(&whole_after)
                .take(inside)
                .map(|(a, b)| compare(a, b).0)
                .fold(0.0, f64::max),
        );
    }
    Ok(check)
}

/// The largest channel difference between two frames, and where it is.
fn compare(
    a: &UniverseState,
    b: &UniverseState,
) -> (f64, Option<(String, &'static str, f64, f64)>) {
    let heads: BTreeSet<&String> = a.primitives.keys().chain(b.primitives.keys()).collect();
    let mut max = 0.0_f64;
    let mut worst = None;
    for head in heads {
        let x = channels(a.primitives.get(head));
        let y = channels(b.primitives.get(head));
        for (channel, (p, q)) in CHANNELS.iter().zip(x.iter().zip(y)) {
            let diff = (p - q).abs();
            if diff > max {
                max = diff;
                worst = Some((head.clone(), *channel, *p, q));
            }
        }
    }
    (max, worst)
}

const CHANNELS: [&str; 6] = ["red", "green", "blue", "strobe", "pan", "tilt"];

/// Emitted light per channel, strobe and aim. Aim is in degrees; a difference
/// of more than a millidegree already counts as different.
fn channels(state: Option<&PrimitiveState>) -> [f64; 6] {
    let Some(state) = state else {
        return [0.0; 6];
    };
    let light = |c: usize| f64::from(state.color[c]) * f64::from(state.dimmer);
    [
        light(0),
        light(1),
        light(2),
        f64::from(state.strobe),
        f64::from(state.position[0]),
        f64::from(state.position[1]),
    ]
}

// ---------------------------------------------------------------------------
// Change set

/// Every row change, by table, with what the apply step must know.
#[derive(Serialize)]
struct ChangeSet {
    snapshot: String,
    generated_at: String,
    /// Read these before applying.
    warnings: Vec<String>,
    counts: BTreeMap<String, BTreeMap<&'static str, usize>>,
    tables: BTreeMap<&'static str, TableChanges>,
    /// Rows the migration would delete but must keep, with why.
    kept: Vec<Json>,
}

async fn change_set(
    pool: &SqlitePool,
    changes: Changes,
    args: &Arguments,
    now: &str,
) -> Result<ChangeSet, String> {
    let query = |sql: &'static str| sqlx::query(sql).fetch_all(pool);
    let mut tables = BTreeMap::new();
    let mut kept = Vec::new();
    let mut warnings = vec![
        "Apply in one transaction on a connection with the sync triggers installed \
         (backend/src/sync/triggers.rs `install`). A plain sqlite3 connection neither logs \
         the rows to `changes` nor queues them for upload, and the next download would \
         bring the old rows back."
            .to_owned(),
        "Every clips update and delete also matches on `uid`; an update that matches no \
         row means the row changed after the snapshot."
            .to_owned(),
    ];
    if args.score.is_some() {
        warnings.push(
            "This run covered one score only; score definitions, patterns and implementations \
             are not listed."
                .into(),
        );
    }

    // Score definitions: every row no remaining clip names, directly or
    // through another definition.
    let mut definitions = TableChanges::default();
    if args.score.is_none() {
        let rows = query("SELECT id, uid, score_id, definition_json FROM score_definitions")
            .await
            .map_err(|e| e.to_string())?;
        let texts: BTreeMap<String, String> = rows
            .iter()
            .map(|row| (row.get("id"), row.get("definition_json")))
            .collect();
        let mut in_use = changes.definitions_in_use.clone();
        loop {
            let found: Vec<String> = texts
                .keys()
                .filter(|id| !in_use.contains(*id))
                .filter(|id| {
                    let (score, key) = id.split_once(':').unwrap_or(("", id));
                    in_use.iter().any(|user| {
                        user.starts_with(&format!("{score}:"))
                            && texts.get(user).is_some_and(|text| {
                                text.contains(&format!("\"definition\":\"{key}\""))
                            })
                    })
                })
                .cloned()
                .collect();
            if found.is_empty() {
                break;
            }
            in_use.extend(found);
        }
        for row in &rows {
            let id: String = row.get("id");
            if in_use.contains(&id) {
                kept.push(json!({
                    "table": "score_definitions",
                    "id": id,
                    "reason": "a clip that was not converted still names it",
                }));
                continue;
            }
            definitions.deletes.push(json!({
                "id": id,
                "uid": row.get::<String, _>("uid"),
                "score_id": row.get::<String, _>("score_id"),
            }));
        }
    }
    tables.insert("score_definitions", definitions);

    // Library patterns, their implementations and cues all go: presets
    // replace patterns, and cues leave the product. Rows that point at them
    // go too, so the commit passes its foreign keys and nothing dangles.
    if args.score.is_none() {
        let rows = |sql: &'static str| query(sql);
        for row in rows("SELECT id, action_json FROM midi_bindings ORDER BY id")
            .await
            .map_err(|e| e.to_string())?
        {
            let action: Json =
                serde_json::from_str(row.get::<&str, _>("action_json")).unwrap_or_default();
            if action["type"] == "fireCue" {
                tables
                    .entry("midi_bindings")
                    .or_default()
                    .deletes
                    .push(json!({
                        "id": row.get::<String, _>("id"),
                        "reason": "fires a cue",
                        "cue_id": action["cue_id"],
                    }));
            }
        }
        for row in rows("SELECT id, uid, name, pattern_id FROM cues ORDER BY id")
            .await
            .map_err(|e| e.to_string())?
        {
            tables.entry("cues").or_default().deletes.push(json!({
                "id": row.get::<String, _>("id"),
                "uid": row.get::<Option<String>, _>("uid"),
                "name": row.get::<String, _>("name"),
                "pattern_id": row.get::<String, _>("pattern_id"),
            }));
        }
        for row in rows(
            "SELECT venue_id, pattern_id, implementation_id FROM venue_implementation_overrides",
        )
        .await
        .map_err(|e| e.to_string())?
        {
            tables
                .entry("venue_implementation_overrides")
                .or_default()
                .deletes
                .push(json!({
                    "venue_id": row.get::<String, _>("venue_id"),
                    "pattern_id": row.get::<String, _>("pattern_id"),
                    "implementation_id": row.get::<String, _>("implementation_id"),
                }));
        }
        for row in rows("SELECT id, uid, name, pattern_id FROM implementations ORDER BY id")
            .await
            .map_err(|e| e.to_string())?
        {
            tables
                .entry("implementations")
                .or_default()
                .deletes
                .push(json!({
                    "id": row.get::<String, _>("id"),
                    "uid": row.get::<Option<String>, _>("uid"),
                    "name": row.get::<Option<String>, _>("name"),
                    "pattern_id": row.get::<String, _>("pattern_id"),
                }));
        }
        for row in rows("SELECT id, uid, name, score_id FROM patterns ORDER BY id")
            .await
            .map_err(|e| e.to_string())?
        {
            tables.entry("patterns").or_default().deletes.push(json!({
                "id": row.get::<String, _>("id"),
                "uid": row.get::<Option<String>, _>("uid"),
                "name": row.get::<String, _>("name"),
                "score_id": row.get::<Option<String>, _>("score_id"),
            }));
        }
        // Agent threads keep their history; only the link goes.
        for row in rows(
            "SELECT id, implementation_id FROM agent_threads \
             WHERE implementation_id IS NOT NULL ORDER BY id",
        )
        .await
        .map_err(|e| e.to_string())?
        {
            tables
                .entry("agent_threads")
                .or_default()
                .updates
                .push(json!({
                    "id": row.get::<String, _>("id"),
                    "set": {"implementation_id": null, "updated_at": now},
                    "was": {"implementation_id": row.get::<String, _>("implementation_id")},
                }));
        }
        warnings.push(
            "Every cue, library pattern and implementation is deleted, with the MIDI bindings \
             that fire a cue, and agent threads lose their implementation link. \
             `pattern_categories` is unaffected: patterns point at categories, not the other \
             way."
                .into(),
        );
    }
    tables.insert("clips", changes.clips);

    let counts = tables
        .iter()
        .map(|(name, table)| {
            (
                (*name).to_owned(),
                BTreeMap::from([
                    ("updates", table.updates.len()),
                    ("inserts", table.inserts.len()),
                    ("deletes", table.deletes.len()),
                ]),
            )
        })
        .collect();
    Ok(ChangeSet {
        snapshot: args.database.display().to_string(),
        generated_at: now.to_owned(),
        warnings,
        counts,
        tables,
        kept,
    })
}

/// The change set as one SQL transaction. It is written, never run.
fn sql(set: &ChangeSet, now: &str) -> String {
    use std::fmt::Write;
    let text = |value: &Json| match value {
        Json::Null => "NULL".to_owned(),
        Json::String(text) => format!("'{}'", text.replace('\'', "''")),
        Json::Number(number) => number.to_string(),
        Json::Bool(value) => i32::from(*value).to_string(),
        other => format!("'{}'", other.to_string().replace('\'', "''")),
    };
    let mut out = String::new();
    writeln!(
        out,
        "-- Clip forms migration, generated {now} from {}.\n\
         -- Not run by the dry run. Read proposed_changes.json `warnings` first.\n\
         PRAGMA foreign_keys = ON;\nBEGIN IMMEDIATE;",
        set.snapshot
    )
    .unwrap();
    let clips = &set.tables["clips"];
    writeln!(out, "\n-- clips: {} updates", clips.updates.len()).unwrap();
    for update in &clips.updates {
        let assignments: Vec<String> = update["set"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(column, value)| format!("{column} = {}", text(value)))
            .collect();
        writeln!(
            out,
            "UPDATE clips SET {} WHERE id = {} AND uid = {};",
            assignments.join(", "),
            text(&update["id"]),
            text(&update["uid"])
        )
        .unwrap();
    }
    writeln!(out, "\n-- clips: {} inserts", clips.inserts.len()).unwrap();
    const COLUMNS: [&str; 14] = [
        "id",
        "uid",
        "score_id",
        "graph",
        "start",
        "duration",
        "seed",
        "selection_seed",
        "selection_json",
        "z_index",
        "blend_mode",
        "inputs_json",
        "created_at",
        "updated_at",
    ];
    for insert in &clips.inserts {
        let values: Vec<String> = COLUMNS.iter().map(|c| text(&insert[*c])).collect();
        writeln!(
            out,
            "INSERT INTO clips ({}) VALUES ({});",
            COLUMNS.join(", "),
            values.join(", ")
        )
        .unwrap();
    }
    writeln!(out, "\n-- clips: {} deletes", clips.deletes.len()).unwrap();
    for delete in &clips.deletes {
        writeln!(
            out,
            "DELETE FROM clips WHERE id = {} AND uid = {};",
            text(&delete["id"]),
            text(&delete["uid"])
        )
        .unwrap();
    }
    let empty = TableChanges::default();
    let table = |name: &str| set.tables.get(name).unwrap_or(&empty);
    let updates = &table("agent_threads").updates;
    writeln!(out, "\n-- agent_threads: {} updates", updates.len()).unwrap();
    for update in updates {
        writeln!(
            out,
            "UPDATE agent_threads SET implementation_id = NULL, updated_at = {} WHERE id = {};",
            text(&update["set"]["updated_at"]),
            text(&update["id"])
        )
        .unwrap();
    }
    // Referencing rows first; the foreign keys are checked at commit anyway.
    for name in [
        "midi_bindings",
        "cues",
        "venue_implementation_overrides",
        "score_definitions",
        "implementations",
        "patterns",
    ] {
        let deletes = &table(name).deletes;
        writeln!(out, "\n-- {name}: {} deletes", deletes.len()).unwrap();
        for delete in deletes {
            let matches = if name == "venue_implementation_overrides" {
                format!(
                    "venue_id = {} AND pattern_id = {}",
                    text(&delete["venue_id"]),
                    text(&delete["pattern_id"])
                )
            } else {
                format!("id = {}", text(&delete["id"]))
            };
            writeln!(out, "DELETE FROM {name} WHERE {matches};").unwrap();
        }
    }
    writeln!(out, "\nCOMMIT;").unwrap();
    out
}

// ---------------------------------------------------------------------------
// Report

fn summary(reports: &[ClipReport], set: &ChangeSet, database: &Path) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let statuses = Status::ALL;
    let count = |filter: &dyn Fn(&ClipReport) -> bool| reports.iter().filter(|r| filter(r)).count();
    writeln!(out, "# Clip forms dry run\n").unwrap();
    writeln!(
        out,
        "Snapshot: `{}`. Clips: {}. Exact < {EXACT:e} per channel; close < {CLOSE}.\n",
        database.display(),
        reports.len()
    )
    .unwrap();
    writeln!(out, "| Status | Clips |\n|---|---:|").unwrap();
    for status in statuses {
        writeln!(
            out,
            "| {} | {} |",
            status.name(),
            count(&|r| r.status == status)
        )
        .unwrap();
    }

    writeln!(out, "\n## Change set\n").unwrap();
    writeln!(
        out,
        "`proposed_changes.json` and `proposed_changes.sql` (one transaction, not run).\n"
    )
    .unwrap();
    writeln!(
        out,
        "| Table | Updates | Inserts | Deletes |\n|---|---:|---:|---:|"
    )
    .unwrap();
    for (table, counts) in &set.counts {
        writeln!(
            out,
            "| {table} | {} | {} | {} |",
            counts["updates"], counts["inserts"], counts["deletes"]
        )
        .unwrap();
    }
    writeln!(out, "\nKept rows: {}.\n", set.kept.len()).unwrap();
    for warning in &set.warnings {
        writeln!(out, "- {warning}").unwrap();
    }

    writeln!(out, "\n## Per form\n").unwrap();
    table(&mut out, reports, |r| {
        r.form.clone().unwrap_or_else(|| "(none)".into())
    });
    writeln!(out, "\n## Per old definition → form\n").unwrap();
    table(&mut out, reports, |r| {
        format!(
            "{} → {}",
            r.definition,
            r.form.as_deref().unwrap_or("(unmatched)")
        )
    });

    writeln!(out, "\n## Reasons for non-exact clips\n").unwrap();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for report in reports.iter().filter(|r| r.status != Status::Exact) {
        for reason in &report.reasons {
            *reasons.entry(generic(reason)).or_default() += 1;
        }
    }
    let mut reasons: Vec<_> = reasons.into_iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    writeln!(out, "| Reason | Clips |\n|---|---:|").unwrap();
    for (reason, count) in reasons {
        writeln!(out, "| {} | {count} |", escape(&reason)).unwrap();
    }

    let moved: Vec<&ClipReport> = reports.iter().filter(|r| r.start_moved < -1e-9).collect();
    writeln!(
        out,
        "\n## Clips that start earlier ({})\n\nThe clip starts on an event that was still lit at the old start. Added light is the most light shown before the old start.\n",
        moved.len()
    )
    .unwrap();
    if !moved.is_empty() {
        writeln!(
            out,
            "| Track | Clip | Definition | Earlier by (beats) | Added light | Status |\n|---|---|---|---:|---:|---|"
        )
        .unwrap();
        for r in moved {
            writeln!(
                out,
                "| {} | `{}` | {} | {:.4} | {:.3} | {} |",
                escape(&r.track),
                r.id,
                escape(&r.definition),
                -r.start_moved,
                r.added_light,
                r.status.name()
            )
            .unwrap();
        }
    }

    let layered: Vec<&ClipReport> = reports.iter().filter(|r| !r.layers.is_empty()).collect();
    writeln!(out, "\n## Layered clips ({})\n", layered.len()).unwrap();
    if !layered.is_empty() {
        writeln!(
            out,
            "Whole score diff: the largest difference of the whole score's light while the clip plays, for clips with a multiply layer over them; it includes the clips that layer darkens.\n\n\
             | Track | Clip | Definition | Look | Converted clip | Layers | Max diff | Whole score diff | Status |\n|---|---|---|---|---|---|---:|---:|---|"
        )
        .unwrap();
        for r in layered {
            writeln!(
                out,
                "| {} | `{}` | {} | {} | {} | {} | {} | {} | {} |",
                escape(&r.track),
                r.id,
                escape(&r.definition),
                escape(r.look.as_deref().unwrap_or("")),
                r.form.as_deref().unwrap_or(""),
                escape(&r.layers.join(", ")),
                r.max_diff.map_or(String::new(), |d| format!("{d:.4}")),
                r.stack_max_diff
                    .map_or(String::new(), |d| format!("{d:.4}")),
                r.status.name()
            )
            .unwrap();
        }
    }

    for status in [
        Status::Unmatched,
        Status::Unrendered,
        Status::Deleted,
        Status::Different,
        Status::Close,
    ] {
        let listed: Vec<&ClipReport> = reports.iter().filter(|r| r.status == status).collect();
        writeln!(out, "\n## {} ({})\n", status.name(), listed.len()).unwrap();
        if listed.is_empty() {
            continue;
        }
        writeln!(
            out,
            "| Track | Clip | Definition | Form | Max diff | Samples ≥ {CLOSE} | Reasons |\n|---|---|---|---|---:|---:|---|"
        )
        .unwrap();
        for r in listed {
            writeln!(
                out,
                "| {} | `{}` | {} | {} | {} | {} | {} |",
                escape(&r.track),
                r.id,
                escape(&r.definition),
                r.form.as_deref().unwrap_or(""),
                r.max_diff.map_or(String::new(), |d| format!("{d:.4}")),
                if r.samples > 0 {
                    format!("{}/{}", r.samples_over_close, r.samples)
                } else {
                    String::new()
                },
                escape(&r.reasons.join("; "))
            )
            .unwrap();
        }
    }
    out
}

/// A reason without its numbers, so equal causes group.
fn generic(reason: &str) -> String {
    let mut out = String::new();
    let mut number = false;
    for c in reason.chars() {
        if c.is_ascii_digit() || (number && c == '.') {
            if !number {
                out.push('N');
            }
            number = true;
        } else {
            number = false;
            out.push(c);
        }
    }
    out
}

fn table(out: &mut String, reports: &[ClipReport], key: impl Fn(&ClipReport) -> String) {
    use std::fmt::Write;
    let mut rows: BTreeMap<String, [usize; 6]> = BTreeMap::new();
    for report in reports {
        rows.entry(key(report)).or_default()[report.status as usize] += 1;
    }
    let mut rows: Vec<_> = rows.into_iter().collect();
    rows.sort_by(|a, b| {
        b.1.iter()
            .sum::<usize>()
            .cmp(&a.1.iter().sum::<usize>())
            .then(a.0.cmp(&b.0))
    });
    writeln!(
        out,
        "| | Clips | Exact | Close | Different | Unmatched | Unrendered | Deleted |\n|---|---:|---:|---:|---:|---:|---:|---:|"
    )
    .unwrap();
    for (key, counts) in rows {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            escape(&key),
            counts.iter().sum::<usize>(),
            counts[0],
            counts[1],
            counts[2],
            counts[3],
            counts[4],
            counts[5]
        )
        .unwrap();
    }
}

fn escape(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

impl Arguments {
    fn parse() -> Result<Self, String> {
        let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let base = home.join("luma-migration/clip-forms");
        let mut parsed = Self {
            database: base.join("luma-snapshot.db"),
            output: base.join("report"),
            storage: match env::var_os("LUMA_CONFIG_DIR") {
                Some(path) => StorageRoot::from_path(path.into()),
                None => StorageRoot::from_env_default()?,
            },
            score: None,
        };
        let mut args = env::args().skip(1);
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| format!("{flag} requires a value"))
            };
            match flag.as_str() {
                "--db" => parsed.database = value()?.into(),
                "--out" => parsed.output = value()?.into(),
                "--storage-root" => parsed.storage = StorageRoot::from_path(value()?.into()),
                "--score" => parsed.score = Some(value()?),
                "--help" | "-h" => {
                    println!(
                        "clip_forms_dry_run [--db SNAPSHOT_DB] [--out REPORT_DIR] [--storage-root DIR] [--score ID]\n\n\
                         Opens the database read-only. Audio for audio sources is read from the storage root."
                    );
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        if parsed.database == parsed.storage.luma_db_path() {
            return Err("run the dry run on a copy, not the live library database".into());
        }
        Ok(parsed)
    }
}
