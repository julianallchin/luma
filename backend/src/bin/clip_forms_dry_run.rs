//! Dry run of the clip-forms migration (docs/specs/clip-forms.md, Migration).
//!
//! Opens a copy of the library database read-only, converts every clip with
//! `luma_lib::migration::clip_forms`, renders the old and the new clip on the
//! clip's own venue and selection, and writes a report: a markdown summary,
//! one JSON file per clip and the proposed new `clips` row values. It writes
//! no database.
//!
//! ```text
//! cargo +1.97.1 run --release --bin clip_forms_dry_run -- \
//!     --db ~/luma-migration/clip-forms/luma-snapshot.db \
//!     --out ~/luma-migration/clip-forms/report
//! ```
use luma_lib::{
    eval::{try_eval, Arena, CompiledAnnotation, Scene},
    migration::clip_forms::{convert, repair_draft_inputs, Converted, Host},
    models::universe::{PrimitiveState, UniverseState},
    storage::StorageRoot,
};
use luma_patterns::{BeatTimeline, Clip, Drum, Score, Selection};
use serde::Serialize;
use serde_json::json;
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
}
impl Status {
    fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Close => "close",
            Self::Different => "different",
            Self::Unmatched => "unmatched",
            Self::Unrendered => "unrendered",
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
    added_light: f64,
    old: serde_json::Value,
    new: Option<serde_json::Value>,
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args = Arguments::parse()?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&args.database)
                .read_only(true),
        )
        .await
        .map_err(|e| format!("cannot open {} read-only: {e}", args.database.display()))?;
    let fixtures = luma_lib::headless_host::HostConfig::default().fixtures_root()?;
    let scores = sqlx::query(
        "SELECT s.id, COALESCE(s.name, '') AS name, s.track_id, \
         COALESCE(t.title, '') AS title FROM scores s JOIN tracks t ON t.id = s.track_id \
         WHERE EXISTS (SELECT 1 FROM clips c WHERE c.score_id = s.id) ORDER BY s.id",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?;
    let clips_dir = args.output.join("clips");
    fs::create_dir_all(&clips_dir).map_err(|e| e.to_string())?;
    let mut reports = Vec::new();
    let mut proposed = Vec::new();
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
            track_id: &track_id,
        };
        let (score_reports, rows) = run.run(&score_name, &title).await?;
        reports.extend(score_reports);
        proposed.extend(rows);
    }
    for report in &reports {
        let file = clips_dir.join(format!("{}.json", report.id.replace([':', '/'], "_")));
        write(&file, &serde_json::to_string_pretty(report).unwrap())?;
    }
    write(
        &args.output.join("proposed_rows.json"),
        &serde_json::to_string_pretty(&proposed).unwrap(),
    )?;
    let summary = summary(&reports, &args.database);
    write(&args.output.join("summary.md"), &summary)?;
    println!("{}", args.output.join("summary.md").display());
    Ok(())
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

struct Loaded {
    score: Score,
    /// Stored selection JSON per clip key, `subset` included.
    selections: BTreeMap<String, serde_json::Value>,
    /// Notes from repairing draft form inputs.
    repairs: BTreeMap<String, Vec<String>>,
    unreadable: BTreeMap<String, String>,
}

struct ScoreRun<'a> {
    pool: &'a SqlitePool,
    storage: &'a StorageRoot,
    fixtures: &'a Path,
    score_id: &'a str,
    track_id: &'a str,
}

impl ScoreRun<'_> {
    async fn run(
        &self,
        score_name: &str,
        track: &str,
    ) -> Result<(Vec<ClipReport>, Vec<serde_json::Value>), String> {
        let Loaded {
            mut score,
            selections,
            repairs,
            unreadable,
        } = self.load().await?;
        // Each clip's z-index becomes its position, so a compiled annotation
        // names its clip.
        for (index, clip) in score.clips.values_mut().enumerate() {
            clip.z_index = index as i64;
        }
        let keys: Vec<String> = score.clips.keys().cloned().collect();
        let grid = luma_lib::services::tracks::get_track_beats(self.pool, self.track_id)
            .await?
            .ok_or("the track has no beat grid")?;
        let clock = grid.timeline().map_err(|e| e.to_string())?;
        let host = self.host(&clock).await?;
        let old = self.scene(&score).await;

        let mut converted: BTreeMap<usize, Result<Converted, String>> = BTreeMap::new();
        for (index, key) in keys.iter().enumerate() {
            let heads = match old.get(&index) {
                Some(Ok(Some(annotation))) => annotation.plan.primitive_ids.len(),
                _ => 0,
            };
            let host = Host {
                heads,
                ..host.clone()
            };
            let selection = selections.get(key).cloned().unwrap_or_default();
            converted.insert(index, convert(&score, key, &selection, &host));
        }
        let mut next = Score::default();
        for (index, key) in keys.iter().enumerate() {
            if let Some(Ok(result)) = converted.get(&index) {
                next.clips.insert(key.clone(), result.clip.clone());
            }
        }
        let new = self.scene(&next).await;

        let mut reports = Vec::new();
        let mut rows = Vec::new();
        for (index, key) in keys.iter().enumerate() {
            let clip = &score.clips[key];
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
                added_light: 0.0,
                old: json!({
                    "start": clip.start,
                    "duration": clip.duration,
                    "graph": clip.graph,
                    "inputs": clip.inputs,
                    "selection": selections.get(key),
                }),
                new: None,
            };
            match &converted[&index] {
                Err(reason) => report.reasons.push(reason.clone()),
                Ok(result) => {
                    report.look = Some(result.look.clone());
                    report.form = Some(result.clip.graph.clone());
                    report.reasons = repairs.get(key).cloned().unwrap_or_default();
                    report.reasons.extend(result.notes.iter().cloned());
                    report.new = Some(json!({
                        "start": result.clip.start,
                        "duration": result.clip.duration,
                        "graph": result.clip.graph,
                        "inputs": result.clip.inputs,
                        "selection": result.clip.selection,
                    }));
                    rows.push(row(&id, self.score_id, clip, &result.clip));
                    let check = check(
                        &clock,
                        clip,
                        result,
                        old.get(&index).unwrap_or(&Ok(None)),
                        new.get(&index).unwrap_or(&Ok(None)),
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
                added_light: 0.0,
                old: serde_json::Value::Null,
                new: None,
            });
        }
        Ok((reports, rows))
    }

    /// The score's rows as the app reads them, with draft form inputs
    /// repaired. A row that still does not read is set aside with its error.
    async fn load(&self) -> Result<Loaded, String> {
        let mut loaded = Loaded {
            score: Score::default(),
            selections: BTreeMap::new(),
            repairs: BTreeMap::new(),
            unreadable: BTreeMap::new(),
        };
        let rows = sqlx::query(
            "SELECT id, graph, start, duration, seed, selection_seed, selection_json, \
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
            let selection: serde_json::Value =
                serde_json::from_str(row.get::<&str, _>("selection_json")).unwrap_or_default();
            let mut inputs: serde_json::Value =
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
            let entries: Vec<serde_json::Value> =
                serde_json::from_str(&json).map_err(|e| e.to_string())?;
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

    /// Every clip compiled, by position. `None` is a clip with no heads on
    /// this venue. A score that fails to compile is compiled one clip at a
    /// time so each failure lands on its own clip.
    async fn scene(
        &self,
        score: &Score,
    ) -> BTreeMap<usize, Result<Option<CompiledAnnotation>, String>> {
        let mut result = BTreeMap::new();
        match self.compile(score).await {
            Ok(scene) => {
                let mut by_z: BTreeMap<i64, CompiledAnnotation> = scene
                    .annotations
                    .into_iter()
                    .map(|annotation| (annotation.z_index, annotation))
                    .collect();
                for clip in score.clips.values() {
                    result.insert(clip.z_index as usize, Ok(by_z.remove(&clip.z_index)));
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
                    result.insert(clip.z_index as usize, compiled);
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

/// The proposed row values of a converted clip.
fn row(
    id: &str,
    score_id: &str,
    old: &luma_patterns::Clip,
    new: &luma_patterns::Clip,
) -> serde_json::Value {
    let mut row = json!({
        "id": id,
        "score_id": score_id,
        "graph": new.graph,
        "inputs_json": serde_json::to_string(&new.inputs).unwrap(),
        "selection_json": serde_json::to_string(&new.selection).unwrap(),
    });
    if new.start != old.start || new.duration != old.duration {
        row["start"] = json!(new.start);
        row["duration"] = json!(new.duration);
    }
    row
}

struct Check {
    max: f64,
    worst: Option<Worst>,
    /// Samples inside the old clip, and how many of them differ by at
    /// least [`CLOSE`].
    samples: usize,
    over_close: usize,
    /// The most light the new clip shows where the old clip was not.
    added: f64,
    bindings_differ: bool,
}

/// Render both clips at evenly spaced beats of the old clip and around every
/// boundary, and find the largest difference of emitted light, strobe or aim.
/// Beats the new clip covers outside the old one are checked apart: there
/// the old clip is dark, and any light is light the conversion adds.
fn check(
    clock: &BeatTimeline,
    old_clip: &luma_patterns::Clip,
    converted: &Converted,
    old: &Result<Option<CompiledAnnotation>, String>,
    new: &Result<Option<CompiledAnnotation>, String>,
) -> Result<Check, String> {
    let old = old
        .as_ref()
        .map_err(|e| format!("old render failed: {e}"))?;
    let new = new
        .as_ref()
        .map_err(|e| format!("new render failed: {e}"))?;
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
    let mut render =
        |annotation: &Option<CompiledAnnotation>| -> Result<Vec<UniverseState>, String> {
            let Some(annotation) = annotation else {
                return Ok(vec![UniverseState::default(); seconds.len()]);
            };
            let frames = try_eval(&annotation.plan, &seconds, &mut arena)?;
            Ok(frames
                .into_iter()
                .zip(&seconds)
                .map(|(frame, t)| {
                    if *t >= annotation.span.0 && *t < annotation.span.1 {
                        frame
                    } else {
                        UniverseState::default()
                    }
                })
                .collect())
        };
    let before = render(old)?;
    let after = render(new)?;
    let mut check = Check {
        max: 0.0,
        worst: None,
        samples: inside,
        over_close: 0,
        added: 0.0,
        bindings_differ: match (old, new) {
            (Some(a), Some(b)) => {
                let bound =
                    |o: &luma_lib::eval::OutputBinding| [o.color || o.dimmer, o.strobe, o.position];
                bound(&a.plan.outputs) != bound(&b.plan.outputs)
            }
            _ => false,
        },
    };
    for (index, ((beat, a), b)) in beats.iter().zip(&before).zip(&after).enumerate() {
        let heads: BTreeSet<&String> = a.primitives.keys().chain(b.primitives.keys()).collect();
        let mut sample = 0.0_f64;
        for head in heads {
            let x = channels(a.primitives.get(head));
            let y = channels(b.primitives.get(head));
            for (channel, (p, q)) in CHANNELS.iter().zip(x.iter().zip(y)) {
                let diff = (p - q).abs();
                sample = sample.max(diff);
                if index < inside && diff > check.max {
                    check.max = diff;
                    check.worst = Some(Worst {
                        beat: *beat,
                        head: head.clone(),
                        channel,
                        old: *p,
                        new: q,
                    });
                }
            }
        }
        if index >= inside {
            check.added = check.added.max(sample);
        } else if sample >= CLOSE {
            check.over_close += 1;
        }
    }
    Ok(check)
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
// Report

fn summary(reports: &[ClipReport], database: &Path) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let statuses = [
        Status::Exact,
        Status::Close,
        Status::Different,
        Status::Unmatched,
        Status::Unrendered,
    ];
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

    for status in [
        Status::Unmatched,
        Status::Unrendered,
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
    let mut rows: BTreeMap<String, [usize; 5]> = BTreeMap::new();
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
        "| | Clips | Exact | Close | Different | Unmatched | Unrendered |\n|---|---:|---:|---:|---:|---:|---:|"
    )
    .unwrap();
    for (key, counts) in rows {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} |",
            escape(&key),
            counts.iter().sum::<usize>(),
            counts[0],
            counts[1],
            counts[2],
            counts[3],
            counts[4]
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
