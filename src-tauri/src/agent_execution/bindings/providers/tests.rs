//! End-to-end provider tests: a synthetic library on a real migrated SQLite
//! database plus a real workspace on disk, assembled through the public
//! [`assemble_bindings`] entry point.
//!
//! These assert the *contract*, not the implementation: manifest paths, tensor
//! shapes and axes, the bytes behind the artifacts, and the exact unavailable
//! reasons — the strings the agent reads when data is missing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

use super::*;
use crate::agent_execution::artifacts::codecs;
use crate::agent_execution::artifacts::ArtifactStore;
use crate::agent_execution::BindingManifest;
use crate::eval::graph_run::{graph_hash, GraphEvaluation, SemanticSignal};
use crate::eval::{Plan, ResidentContext};
use crate::models::node_graph::{Edge, Graph, NodeInstance, Signal};
use crate::storage::StorageRoot;

const TRACK_ID: &str = "trk-1";
const TRACK_HASH: &str = "hash1";
const SCORE_ID: &str = "sc-1";
const PATTERN_ID: &str = "pat-1";

/// Temp-file pool built the way `init_app_db` builds the real one.
async fn test_pool(dir: &Path) -> SqlitePool {
    let db_path = dir.join("luma-test.db");
    let migrate_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .expect("migrate pool");
    sqlx::migrate!("./migrations")
        .run(&migrate_pool)
        .await
        .expect("migrations");
    migrate_pool.close().await;

    SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .expect("pool")
}

/// One synthetic library: a track with every analysis artifact, a venue with two
/// fixtures in a group, an authored timeline with one clip, one pattern.
struct Fixture {
    _dir: tempfile::TempDir,
    pool: SqlitePool,
    storage: StorageRoot,
    resource_root: PathBuf,
    workspace: PathBuf,
    venue_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(dir.path()).await;
        let storage = StorageRoot::from_path(dir.path().join("config"));
        let resource_root = dir.path().join("resources");
        std::fs::create_dir_all(&resource_root).unwrap();
        let workspace = dir.path().join("workspace");
        // Unique per test: `services::groups` keeps a process-wide venue cache.
        let venue_id = format!("ven-{}", uuid::Uuid::new_v4());

        let f = Self {
            _dir: dir,
            pool,
            storage,
            resource_root,
            workspace,
            venue_id,
        };
        f.seed_track().await;
        f.seed_venue().await;
        f.seed_patterns_and_score().await;
        f
    }

    fn store(&self) -> ArtifactStore {
        ArtifactStore::open(&self.workspace).expect("workspace")
    }

    fn scope(&self) -> BindingScope {
        BindingScope {
            agent_kind: "track_copilot".into(),
            track_id: Some(TRACK_ID.into()),
            venue_id: Some(self.venue_id.clone()),
            score_id: Some(SCORE_ID.into()),
            track_editable: true,
            pattern_id: None,
            window: Some((0.0, 30.0)),
            graph_definition: None,
        }
    }

    async fn assemble(&self, scope: &BindingScope) -> (BindingManifest, ArtifactStore) {
        self.assemble_with(scope, None).await
    }

    async fn assemble_with(
        &self,
        scope: &BindingScope,
        run: Option<&GraphRunContribution>,
    ) -> (BindingManifest, ArtifactStore) {
        let mut store = self.store();
        let manifest = assemble_bindings(
            &self.pool,
            &self.storage,
            &self.resource_root,
            scope,
            run,
            &mut store,
        )
        .await
        .expect("assemble");
        (manifest, store)
    }

    // -- seeding ------------------------------------------------------------

    async fn seed_track(&self) {
        let audio_path = self.storage.tracks_dir().join(format!("{TRACK_HASH}.wav"));
        std::fs::create_dir_all(self.storage.tracks_dir()).unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, track_hash, title, artist, album, duration_seconds, file_path)
             VALUES (?, ?, 'Hex', 'Surgeon', 'Force + Form', 200.0, ?)",
        )
        .bind(TRACK_ID)
        .bind(TRACK_HASH)
        .bind(audio_path.to_string_lossy().to_string())
        .execute(&self.pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO track_beats (track_id, beats_json, downbeats_json, bpm, beats_per_bar)
             VALUES (?, '[0.5,1.0,1.5,2.0]', '[0.5,2.5]', 128.0, 4)",
        )
        .bind(TRACK_ID)
        .execute(&self.pool)
        .await
        .unwrap();

        // `hat` is singular here; the bar classifier's tag is `hats`. Both
        // spellings appear in one manifest on purpose.
        sqlx::query("INSERT INTO track_drum_onsets (track_id, onsets_json) VALUES (?, ?)")
            .bind(TRACK_ID)
            .bind(r#"{"kick":[0.5,1.5],"snare":[1.0],"hat":[]}"#)
            .execute(&self.pool)
            .await
            .unwrap();

        // The real shape: `intensity` is a continuous regression value mixed
        // into the same map as the sigmoid tags.
        let classifications = json!([
            {"bar_idx": 0, "start": 0.032, "end": 1.81,
             "predictions": {"intensity": 1.647, "kick": 0.101, "hats": 0.326}},
            {"bar_idx": 1, "start": 1.81, "end": 3.59,
             "predictions": {"intensity": 4.2, "kick": 0.9, "hats": 0.75}}
        ]);
        sqlx::query(
            "INSERT INTO track_bar_classifications (track_id, classifications_json, tag_order_json)
             VALUES (?, ?, '[\"kick\",\"hats\"]')",
        )
        .bind(TRACK_ID)
        .bind(classifications.to_string())
        .execute(&self.pool)
        .await
        .unwrap();

        sqlx::query("INSERT INTO track_roots (track_id, sections_json) VALUES (?, ?)")
            .bind(TRACK_ID)
            .bind(
                r#"[{"start":0.0,"end":2.0,"root":9,"label":"A:maj"},
                    {"start":2.0,"end":4.0,"root":null,"label":"N"}]"#,
            )
            .execute(&self.pool)
            .await
            .unwrap();

        // Only the drums stem exists, and only stems that ran have rows.
        sqlx::query(
            "INSERT INTO track_stems (track_id, stem_name, file_path) VALUES (?, 'drums', ?)",
        )
        .bind(TRACK_ID)
        .bind("drums.ogg")
        .execute(&self.pool)
        .await
        .unwrap();
        crate::preprocessing::failures::record(
            &self.pool,
            TRACK_ID,
            "stems",
            1,
            "demucs ran out of memory",
        )
        .await
        .unwrap();

        // Waveform bands: 4 buckets per band over an 8-second decode.
        let bands: Vec<f32> = vec![
            0.1, 0.2, 0.3, 0.4, // low
            0.5, 0.6, 0.7, 0.8, // mid
            0.9, 1.0, 0.0, 0.05, // high
        ];
        let blob = crate::services::waveforms::f32_slice_to_bytes(&bands);
        crate::database::local::waveforms::upsert_track_waveform(
            &self.pool,
            TRACK_ID,
            &crate::services::waveforms::f32_slice_to_bytes(&[0.0, 0.0]),
            &[],
            &[],
            &[],
            &blob,
            &[],
            48_000,
            8.0,
        )
        .await
        .unwrap();

        // PCM caches: the mix (stereo, 3 frames) and the drums stem (mono).
        crate::audio::cache::write_pcm_file(
            &self.storage.mix_pcm_path(TRACK_HASH),
            &[0.0, 0.1, 0.2, 0.3, 0.4, 0.5],
            48_000,
            2,
        )
        .unwrap();
        crate::audio::cache::write_pcm_file(
            &self.storage.stem_pcm_path(TRACK_HASH, "drums"),
            &[0.0, 1.0, -1.0, 0.5],
            48_000,
            1,
        )
        .unwrap();

        // MERT: real .npy files, shape read back off their own headers.
        let fullmix = self.storage.mert_fullmix_path(TRACK_HASH);
        let drum = self.storage.mert_drum_path(TRACK_HASH);
        std::fs::create_dir_all(self.storage.mert_dir()).unwrap();
        codecs::write_npy_f32(&fullmix, &[0.0f32; 12], &[3, 4]).unwrap();
        codecs::write_npy_f32(&drum, &[1.0f32; 12], &[3, 4]).unwrap();
        sqlx::query("INSERT INTO track_mert (track_id, file_path, drum_path) VALUES (?, ?, ?)")
            .bind(TRACK_ID)
            .bind(fullmix.to_string_lossy().to_string())
            .bind(drum.to_string_lossy().to_string())
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn seed_venue(&self) {
        sqlx::query("INSERT INTO venues (id, name) VALUES (?, 'Basement')")
            .bind(&self.venue_id)
            .execute(&self.pool)
            .await
            .unwrap();
        for (i, id) in ["fix-a", "fix-b"].iter().enumerate() {
            sqlx::query(
                "INSERT INTO fixtures (id, venue_id, universe, address, num_channels,
                    manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z)
                 VALUES (?, ?, 1, ?, 8, 'Chauvet', 'SlimPAR', '8-Channel',
                    'Chauvet/SlimPAR.qxf', ?, ?, 0.0, 2.0)",
            )
            .bind(id)
            .bind(&self.venue_id)
            .bind(1 + i as i64 * 8)
            .bind(format!("PAR {}", i + 1))
            .bind(i as f64)
            .execute(&self.pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO fixture_groups (id, venue_id, name, axis_lr, display_order)
             VALUES ('grp-1', ?, 'front_wash', -1.0, 0)",
        )
        .bind(&self.venue_id)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO fixture_group_members (id, fixture_id, group_id, head_index)
             VALUES ('mem-1', 'fix-a', 'grp-1', -1)",
        )
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn seed_patterns_and_score(&self) {
        sqlx::query(
            "INSERT INTO patterns (id, name, description, category_name, is_verified)
             VALUES (?, 'Strobe', 'a strobe', 'Effects', 1)",
        )
        .bind(PATTERN_ID)
        .execute(&self.pool)
        .await
        .unwrap();
        let graph = json!({
            "nodes": [], "edges": [],
            "args": [{"id": "color", "name": "Color", "argType": "Color",
                      "defaultValue": {"r": 1.0}}]
        });
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, graph_json) VALUES ('imp-1', ?, ?)",
        )
        .bind(PATTERN_ID)
        .bind(graph.to_string())
        .execute(&self.pool)
        .await
        .unwrap();

        sqlx::query("INSERT INTO scores (id, track_id, venue_id, name) VALUES (?, ?, ?, 'Main')")
            .bind(SCORE_ID)
            .bind(TRACK_ID)
            .bind(&self.venue_id)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track_scores
                (id, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json)
             VALUES ('ann-1', ?, ?, 12.5, 20.0, 3, 'add', '{\"intensity\":0.5}')",
        )
        .bind(SCORE_ID)
        .bind(PATTERN_ID)
        .execute(&self.pool)
        .await
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn root(manifest: &BindingManifest) -> Value {
    serde_json::to_value(&manifest.root).unwrap()
}

/// Walk a dotted path through the serialized manifest root.
fn at<'a>(v: &'a Value, path: &str) -> &'a Value {
    let mut cursor = v;
    for segment in path.split('.') {
        cursor = cursor
            .get(segment)
            .unwrap_or_else(|| panic!("missing binding path '{path}' (at '{segment}')"));
    }
    cursor
}

fn reason(v: &Value, path: &str) -> String {
    let node = at(v, path);
    assert_eq!(node["$kind"], "unavailable", "expected {path} unavailable");
    node["reason"].as_str().unwrap().to_string()
}

fn shape(v: &Value, path: &str) -> Vec<usize> {
    let node = at(v, path);
    assert_eq!(node["$kind"], "tensor", "expected {path} to be a tensor");
    serde_json::from_value(node["shape"].clone()).unwrap()
}

/// Read the f32s a tensor binding points at, straight out of the workspace.
fn read_f32(manifest: &BindingManifest, store: &ArtifactStore, path: &str) -> Vec<f32> {
    read_raw(manifest, store, path, 4)
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

fn read_f64(manifest: &BindingManifest, store: &ArtifactStore, path: &str) -> Vec<f64> {
    read_raw(manifest, store, path, 8)
        .chunks_exact(8)
        .map(|b| f64::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

fn read_raw(
    manifest: &BindingManifest,
    store: &ArtifactStore,
    path: &str,
    width: usize,
) -> Vec<u8> {
    let node = at(&root(manifest), path).clone();
    let id = crate::agent_execution::ArtifactId::from_string(
        node["artifact_id"].as_str().expect("artifact_id"),
    );
    let descriptor = manifest.artifacts.get(&id).expect("descriptor");
    let file = store.resolve(&descriptor.rel_path).unwrap();
    let bytes = std::fs::read(file).unwrap();
    let offset = node["byte_offset"].as_u64().unwrap() as usize;
    let count: usize = serde_json::from_value::<Vec<usize>>(node["shape"].clone())
        .unwrap()
        .iter()
        .product();
    bytes[offset..offset + count * width].to_vec()
}

// ---------------------------------------------------------------------------
// Shape of the whole tree
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_assembly_covers_every_schema_branch() {
    let f = Fixture::new().await;
    let (manifest, _store) = f.assemble(&f.scope()).await;
    let v = root(&manifest);

    // §10.1 track
    assert_eq!(at(&v, "track.id"), TRACK_ID);
    assert_eq!(at(&v, "track.title"), "Hex");
    assert_eq!(at(&v, "track.artist"), "Surgeon");
    assert_eq!(at(&v, "track.duration_s"), 200.0);
    assert_eq!(at(&v, "track.bpm"), 128.0);
    assert_eq!(at(&v, "track.key")["$kind"], "unavailable");

    // §10.1 audio
    assert_eq!(shape(&v, "audio.mix"), vec![3, 2]);
    assert_eq!(at(&v, "audio.mix")["byte_offset"], 18);
    assert_eq!(shape(&v, "audio.stems.drums"), vec![4]);
    for stem in ["bass", "vocals", "other"] {
        assert_eq!(
            at(&v, &format!("audio.stems.{stem}"))["$kind"],
            "unavailable"
        );
    }

    // §10.1 features
    assert_eq!(shape(&v, "features.beats"), vec![4]);
    assert_eq!(shape(&v, "features.downbeats"), vec![2]);
    assert_eq!(at(&v, "features.bpm"), 128.0);
    assert_eq!(at(&v, "features.beats_per_bar"), 4);
    assert_eq!(shape(&v, "features.drum_onsets.kick"), vec![2]);
    assert_eq!(shape(&v, "features.drum_onsets.snare"), vec![1]);
    assert_eq!(shape(&v, "features.drum_onsets.hat"), vec![0]);
    assert_eq!(shape(&v, "features.bars.indices"), vec![2]);
    assert_eq!(shape(&v, "features.bars.starts_s"), vec![2]);
    assert_eq!(shape(&v, "features.bars.ends_s"), vec![2]);
    assert_eq!(shape(&v, "features.bars.intensity"), vec![2]);
    assert_eq!(shape(&v, "features.bars.predictions"), vec![2, 2]);
    assert_eq!(at(&v, "features.bars.tags")[0], "kick");
    assert!(at(&v, "features.bars.thresholds")["vocal_chop"].is_number());
    assert_eq!(shape(&v, "features.chords.starts_s"), vec![2]);
    assert_eq!(shape(&v, "features.waveform_bands"), vec![3, 4]);
    assert_eq!(shape(&v, "features.mert.fullmix"), vec![3, 4]);
    assert_eq!(shape(&v, "features.mert.drum"), vec![3, 4]);

    // §10.1 venue
    assert_eq!(at(&v, "venue.name"), "Basement");
    assert_eq!(at(&v, "venue.fixtures").as_array().unwrap().len(), 2);
    assert_eq!(at(&v, "venue.groups")[0]["name"], "front_wash");
    assert_eq!(shape(&v, "venue.positions"), vec![2, 3]);

    // §10.2 authored timeline + patterns. Persistence vocabulary (`score`)
    // deliberately does not leak into the agent namespace.
    assert!(v.get("score").is_none());
    assert_eq!(at(&v, "track.editable"), true);
    let revision = at(&v, "track.revision").as_str().unwrap();
    let stored = crate::database::local::scores::list_track_scores_for_score(&f.pool, SCORE_ID)
        .await
        .unwrap();
    assert_eq!(
        revision,
        crate::services::track_edits::track_revision(&stored)
    );
    let clip = &at(&v, "track.clips")[0];
    assert_eq!(clip["id"], "ann-1");
    assert_eq!(clip["pattern_id"], PATTERN_ID);
    assert_eq!(clip["pattern_name"], "Strobe");
    assert_eq!(clip["start_s"], 12.5);
    assert_eq!(clip["end_s"], 20.0);
    assert_eq!(clip["z"], 3);
    assert_eq!(clip["blend"], "add");
    assert_eq!(clip["args"]["intensity"], 0.5);
    assert_eq!(at(&v, "patterns.summaries")[0]["name"], "Strobe");
    assert_eq!(
        at(&v, "patterns.argument_schemas")[PATTERN_ID][0]["id"],
        "color"
    );

    // §10.3 graph: nothing was contributed.
    assert_eq!(at(&v, "graph.run")["$kind"], "unavailable");
    assert_eq!(at(&v, "graph.definition")["$kind"], "unavailable");

    // The worker synthesizes these; the host must not emit them (appendix A.4).
    assert!(v.get("meta").is_none());
    assert!(v.get("window").is_none());

    // The envelope carries the scope instead.
    assert_eq!(manifest.scope.track_id.as_deref(), Some(TRACK_ID));
    assert_eq!(manifest.scope.window.unwrap().end_s, 30.0);
}

#[tokio::test]
async fn serialized_manifest_reads_as_contract_c1() {
    let f = Fixture::new().await;
    let (manifest, _store) = f.assemble(&f.scope()).await;
    let json: Value = serde_json::from_str(&manifest.to_json().unwrap()).unwrap();

    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["agent_kind"], "track_copilot");
    assert!(json["revision"].as_str().unwrap().starts_with("r-"));
    assert_eq!(json["root"]["features"]["beats"]["$kind"], "tensor");
    assert_eq!(json["root"]["features"]["beats"]["unit"], "s");
    assert_eq!(
        json["root"]["features"]["beats"]["axes"][0]["kind"],
        "index"
    );
    assert_eq!(json["root"]["features"]["mel"]["$kind"], "unavailable");
    assert_eq!(json["root"]["track"]["title"], "Hex");

    // Every tensor resolves to a published artifact with a workspace-relative path.
    let artifacts = json["artifacts"].as_object().unwrap();
    assert!(!artifacts.is_empty());
    for descriptor in artifacts.values() {
        let rel = descriptor["rel_path"].as_str().unwrap();
        assert!(rel.starts_with("inputs/"), "{rel}");
    }
    let mix = &json["root"]["audio"]["mix"];
    let mix_artifact = &artifacts[mix["artifact_id"].as_str().unwrap()];
    assert_eq!(mix_artifact["encoding"], "pcm_f32");
    assert_eq!(mix_artifact["sample_rate_hz"], 48000);
    assert_eq!(mix_artifact["channels"], 2);
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bar_intensity_is_split_out_of_the_tag_predictions() {
    let f = Fixture::new().await;
    let (manifest, store) = f.assemble(&f.scope()).await;
    let v = root(&manifest);

    let intensity = read_f64(&manifest, &store, "features.bars.intensity");
    assert_eq!(intensity, vec![1.647, 4.2]);

    // [bar, tag] in tag_order — and intensity is nowhere in it.
    let predictions = read_f32(&manifest, &store, "features.bars.predictions");
    assert_eq!(predictions, vec![0.101, 0.326, 0.9, 0.75]);
    let tags = at(&v, "features.bars.predictions")["axes"][1].clone();
    assert_eq!(tags["kind"], "labels");
    assert_eq!(tags["labels"], json!(["kick", "hats"]));

    // The bar axis carries start times, so a bar row can be placed in time.
    let bar_axis = &at(&v, "features.bars.predictions")["axes"][0];
    assert_eq!(bar_axis["kind"], "coordinates");
    assert_eq!(bar_axis["values"], json!([0.032, 1.81]));
    assert_eq!(bar_axis["unit"], "s");

    assert_eq!(
        read_f64(&manifest, &store, "features.bars.starts_s"),
        vec![0.032, 1.81]
    );
    assert_eq!(
        read_f32(&manifest, &store, "features.beats"),
        vec![0.5, 1.0, 1.5, 2.0]
    );
}

#[tokio::test]
async fn a_chord_section_without_a_root_is_nan_not_a_sentinel() {
    let f = Fixture::new().await;
    let (manifest, store) = f.assemble(&f.scope()).await;
    let roots = read_f64(&manifest, &store, "features.chords.root_pitch_class");
    assert_eq!(roots[0], 9.0);
    assert!(
        roots[1].is_nan(),
        "missing root must be NaN, got {}",
        roots[1]
    );
    // …and the label survives, which is where "no chord" is actually readable.
    let v = root(&manifest);
    assert_eq!(at(&v, "features.chords.labels"), &json!(["A:maj", "N"]));
}

#[tokio::test]
async fn waveform_band_times_come_from_the_decoded_duration() {
    let f = Fixture::new().await;
    let (manifest, store) = f.assemble(&f.scope()).await;
    let v = root(&manifest);
    let axes = &at(&v, "features.waveform_bands")["axes"];
    assert_eq!(axes[0]["labels"], json!(["low", "mid", "high"]));
    // 4 buckets over the 8 s decode ⇒ centers at 1, 3, 5, 7.
    assert_eq!(axes[1]["kind"], "linear");
    assert_eq!(axes[1]["start"], 1.0);
    assert_eq!(axes[1]["step"], 2.0);
    assert_eq!(axes[1]["count"], 4);
    assert_eq!(axes[1]["unit"], "s");

    let data = read_f32(&manifest, &store, "features.waveform_bands");
    assert_eq!(&data[..4], &[0.1, 0.2, 0.3, 0.4]);
    assert_eq!(&data[8..], &[0.9, 1.0, 0.0, 0.05]);
}

#[tokio::test]
async fn audio_tensors_describe_their_own_pcm_layout() {
    let f = Fixture::new().await;
    let (manifest, store) = f.assemble(&f.scope()).await;
    let v = root(&manifest);

    let mix = at(&v, "audio.mix");
    assert_eq!(mix["axes"][0]["kind"], "linear");
    assert_eq!(mix["axes"][0]["unit"], "s");
    assert_eq!(mix["axes"][0]["step"], 1.0 / 48000.0);
    assert_eq!(mix["axes"][1]["labels"], json!(["l", "r"]));
    assert_eq!(
        read_f32(&manifest, &store, "audio.mix"),
        vec![0.0, 0.1, 0.2, 0.3, 0.4, 0.5]
    );

    // Mono stems have no channel axis at all.
    let drums = at(&v, "audio.stems.drums");
    assert_eq!(drums["axes"].as_array().unwrap().len(), 1);
    assert_eq!(
        read_f32(&manifest, &store, "audio.stems.drums"),
        vec![0.0, 1.0, -1.0, 0.5]
    );
}

#[tokio::test]
async fn mert_shape_and_dtype_are_read_off_the_npy_header() {
    let f = Fixture::new().await;
    let (manifest, _store) = f.assemble(&f.scope()).await;
    let v = root(&manifest);
    let mert = at(&v, "features.mert.fullmix");
    assert_eq!(mert["dtype"], "f32"); // whatever the file says — f16 in production
    assert_eq!(mert["byte_offset"], 0);
    assert_eq!(mert["axes"][0]["kind"], "linear");
    assert_eq!(mert["axes"][0]["step"], 1.0 / 75.0);
    assert_eq!(mert["axes"][1]["kind"], "index");
}

// ---------------------------------------------------------------------------
// Unavailable versus empty
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unavailable_reasons_name_the_real_cause() {
    let f = Fixture::new().await;
    let (manifest, _store) = f.assemble(&f.scope()).await;
    let v = root(&manifest);

    // A failed preprocessor reports its own error, not a generic string.
    let bass = reason(&v, "audio.stems.bass");
    assert!(bass.contains("demucs ran out of memory"), "{bass}");

    // A class the model didn't emit is unavailable; a class it emitted with no
    // hits is an empty tensor. That distinction is the point.
    assert!(reason(&v, "features.drum_onsets.cymbal").contains("did not emit"));
    assert_eq!(shape(&v, "features.drum_onsets.hat"), vec![0]);

    assert!(reason(&v, "track.key").contains("no key data source"));
    assert!(reason(&v, "features.mel").contains("librosa"));
    assert!(reason(&v, "graph.run").contains("no graph run"));
}

#[tokio::test]
async fn a_separated_stem_without_a_pcm_cache_says_it_is_undecoded() {
    let f = Fixture::new().await;
    std::fs::remove_file(f.storage.stem_pcm_path(TRACK_HASH, "drums")).unwrap();
    std::fs::create_dir_all(f.storage.stems_dir(TRACK_HASH)).unwrap();
    std::fs::write(f.storage.stems_dir(TRACK_HASH).join("drums.ogg"), b"x").unwrap();

    let (manifest, _store) = f.assemble(&f.scope()).await;
    let r = reason(&root(&manifest), "audio.stems.drums");
    assert!(r.contains("no decoded PCM cache"), "{r}");
}

#[tokio::test]
async fn a_track_with_no_analysis_reports_has_not_run() {
    let f = Fixture::new().await;
    sqlx::query("DELETE FROM track_beats WHERE track_id = ?")
        .bind(TRACK_ID)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM track_drum_onsets WHERE track_id = ?")
        .bind(TRACK_ID)
        .execute(&f.pool)
        .await
        .unwrap();
    let (manifest, _store) = f.assemble(&f.scope()).await;
    let v = root(&manifest);

    assert!(reason(&v, "features.beats").contains("has not run"));
    assert!(reason(&v, "features.bpm").contains("has not run"));
    assert!(reason(&v, "features.drum_onsets").contains("has not run"));
    // track.bpm falls back to null rather than lying.
    assert!(at(&v, "track.bpm").is_null());
}

#[tokio::test]
async fn a_scope_with_no_track_marks_every_track_branch_unavailable() {
    let f = Fixture::new().await;
    let scope = BindingScope {
        agent_kind: "pattern_graph".into(),
        track_id: None,
        venue_id: Some(f.venue_id.clone()),
        score_id: None,
        track_editable: false,
        pattern_id: Some(PATTERN_ID.into()),
        window: None,
        graph_definition: None,
    };
    let (manifest, _store) = f.assemble(&scope).await;
    let v = root(&manifest);

    for path in ["track", "audio.mix", "features.beats", "features.mert"] {
        assert!(reason(&v, path).contains("no track"), "{path}");
    }
    assert!(v.get("score").is_none());
    // The venue is still fully there.
    assert_eq!(shape(&v, "venue.positions"), vec![2, 3]);
}

#[tokio::test]
async fn timeline_visibility_does_not_imply_edit_authorization() {
    let f = Fixture::new().await;
    let mut scope = f.scope();
    scope.track_editable = false;

    let (manifest, _store) = f.assemble(&scope).await;
    let v = root(&manifest);

    assert_eq!(at(&v, "track.clips")[0]["id"], "ann-1");
    assert!(at(&v, "track.revision").is_string());
    assert_eq!(at(&v, "track.editable"), false);
}

#[tokio::test]
async fn a_track_without_a_selected_timeline_is_readable_but_not_editable() {
    let f = Fixture::new().await;
    let mut scope = f.scope();
    scope.score_id = None;
    // Even a mistakenly permissive caller bit cannot make an absent timeline
    // editable; the provider only publishes true for a validated scope.
    scope.track_editable = true;

    let (manifest, _store) = f.assemble(&scope).await;
    let v = root(&manifest);

    assert_eq!(at(&v, "track.id"), TRACK_ID);
    assert!(reason(&v, "track.revision").contains("no authored lighting timeline"));
    assert!(reason(&v, "track.clips").contains("no authored lighting timeline"));
    assert_eq!(at(&v, "track.editable"), false);
    assert!(v.get("score").is_none());
}

#[tokio::test]
async fn a_scope_with_no_venue_marks_the_venue_branch_unavailable() {
    let f = Fixture::new().await;
    let mut scope = f.scope();
    scope.venue_id = None;
    let (manifest, _store) = f.assemble(&scope).await;
    let v = root(&manifest);
    for path in ["venue.id", "venue.name", "venue.positions"] {
        assert!(reason(&v, path).contains("no venue"), "{path}");
    }
}

#[tokio::test]
async fn missing_mix_cache_with_a_missing_source_file_says_so() {
    let f = Fixture::new().await;
    std::fs::remove_file(f.storage.mix_pcm_path(TRACK_HASH)).unwrap();
    let (manifest, _store) = f.assemble(&f.scope()).await;
    let r = reason(&root(&manifest), "audio.mix");
    assert!(r.contains("missing from disk"), "{r}");
}

// ---------------------------------------------------------------------------
// Venue identity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn position_rows_are_labeled_with_evaluator_primitive_ids() {
    let f = Fixture::new().await;
    let (manifest, store) = f.assemble(&f.scope()).await;
    let v = root(&manifest);

    let axes = &at(&v, "venue.positions")["axes"];
    assert_eq!(axes[0]["kind"], "labels");
    assert_eq!(axes[0]["labels"], json!(["fix-a:0", "fix-b:0"]));
    assert_eq!(axes[1]["labels"], json!(["x", "y", "z"]));

    // …and they are exactly what the evaluator would resolve, in its order.
    let expected: Vec<String> = crate::eval::context::resolve_primitive_ids(
        &f.pool,
        &f.venue_id,
        &f.resource_root,
        &[],
        &[],
        &HashMap::new(),
    )
    .await
    .into_iter()
    .map(|(id, _)| id)
    .collect();
    assert_eq!(
        axes[0]["labels"],
        serde_json::to_value(&expected).unwrap(),
        "position labels must be the evaluator's own primitive ids"
    );

    let positions = read_f32(&manifest, &store, "venue.positions");
    assert_eq!(positions.len(), 6);
    assert_eq!(positions[2], 2.0); // z of the first fixture
}

// ---------------------------------------------------------------------------
// Graph run compatibility
// ---------------------------------------------------------------------------

fn demo_graph() -> Graph {
    Graph {
        nodes: vec![NodeInstance {
            id: "s0".into(),
            type_id: "scalar".into(),
            params: [("value".to_string(), json!(1.0))].into_iter().collect(),
            position_x: None,
            position_y: None,
        }],
        edges: vec![Edge {
            id: "e0".into(),
            from_node: "s0".into(),
            from_port: "out".into(),
            to_node: "view_signal_1".into(),
            to_port: "in".into(),
        }],
        args: vec![],
    }
}

/// A hand-built evaluation: `evaluate_graph` needs a compiled plan and a venue
/// on disk, and neither is what the compatibility gate is about.
fn evaluation(graph: &Graph, venue_id: &str, span: (f32, f32)) -> GraphEvaluation {
    let primitive_ids = vec!["fix-a:0".to_string(), "fix-b:0".to_string()];
    let times_s: Vec<f32> = (0..4)
        .map(|i| span.0 + (span.1 - span.0) * i as f32 / 3.0)
        .collect();
    let signal = Signal {
        n: 2,
        t: 4,
        c: 3,
        data: (0..24).map(|i| i as f32).collect(),
    };
    let mut views = HashMap::new();
    views.insert(
        "view_signal_1".to_string(),
        SemanticSignal {
            signal,
            channels: vec!["r".into(), "g".into(), "b".into()],
        },
    );
    GraphEvaluation {
        plan: Arc::new(Plan {
            ops: Vec::new(),
            slots: Vec::new(),
            slot_channels: Vec::new(),
            n: 2,
            primitive_ids: primitive_ids.clone(),
            outputs: Default::default(),
            ctx: ResidentContext {
                span,
                ..Default::default()
            },
            prologue_baked: Vec::new(),
            views: Vec::new(),
        }),
        views,
        mel_views: None,
        times_s,
        positions: vec![[0.0, 0.0, 2.0], [1.0, 0.0, 2.0]],
        primitive_ids,
        span,
        graph_hash: graph_hash(graph),
        arg_hash: "arg".into(),
        selection_hash: "sel".into(),
        track_id: TRACK_ID.into(),
        venue_id: venue_id.into(),
        universe_state: None,
    }
}

#[tokio::test]
async fn a_matching_graph_run_is_published_with_identity_on_every_axis() {
    let f = Fixture::new().await;
    let graph = demo_graph();
    let mut scope = f.scope();
    scope.agent_kind = "pattern_graph".into();
    scope.window = Some((0.0, 3.0));
    scope.graph_definition = Some(serde_json::to_value(&graph).unwrap());
    let contribution =
        GraphRunContribution::new(Arc::new(evaluation(&graph, &f.venue_id, (0.0, 3.0))));

    let (manifest, store) = f.assemble_with(&scope, Some(&contribution)).await;
    let v = root(&manifest);

    let view = at(&v, "graph.run.views.view_signal_1");
    assert_eq!(view["$kind"], "tensor");
    assert_eq!(shape(&v, "graph.run.views.view_signal_1"), vec![2, 4, 3]);
    assert_eq!(view["axes"][0]["labels"], json!(["fix-a:0", "fix-b:0"]));
    assert_eq!(view["axes"][1]["kind"], "linear");
    assert_eq!(view["axes"][1]["start"], 0.0);
    assert_eq!(view["axes"][1]["count"], 4);
    assert_eq!(view["axes"][1]["unit"], "s");
    assert_eq!(view["axes"][2]["labels"], json!(["r", "g", "b"]));
    // [n][t][c] row-major, straight through.
    let data = read_f32(&manifest, &store, "graph.run.views.view_signal_1");
    assert_eq!(data[..3], [0.0, 1.0, 2.0]);
    assert_eq!(data[23], 23.0);

    assert_eq!(
        at(&v, "graph.run.primitive_ids"),
        &json!(["fix-a:0", "fix-b:0"])
    );
    assert_eq!(shape(&v, "graph.run.positions"), vec![2, 3]);
    assert_eq!(at(&v, "graph.run.span.end_s"), 3.0);
    assert_eq!(
        at(&v, "graph.run.fingerprints.graph"),
        &json!(graph_hash(&graph))
    );
    assert!(reason(&v, "graph.run.mel_views").contains("not computed"));

    // The definition branch mirrors what the editor sent.
    assert_eq!(at(&v, "graph.definition.nodes")[0]["id"], "s0");
    assert_eq!(at(&v, "graph.definition.nodes")[0]["type"], "scalar");
    assert_eq!(at(&v, "graph.definition.edges")[0]["to_port"], "in");

    // The venue positions and the run's primitives are the same universe.
    assert_eq!(
        at(&v, "venue.positions")["axes"][0]["labels"],
        at(&v, "graph.run.positions")["axes"][0]["labels"]
    );
}

#[tokio::test]
async fn an_edited_graph_invalidates_its_own_run() {
    let f = Fixture::new().await;
    let graph = demo_graph();
    let contribution =
        GraphRunContribution::new(Arc::new(evaluation(&graph, &f.venue_id, (0.0, 3.0))));

    let mut edited = graph.clone();
    edited.nodes[0].params.insert("value".into(), json!(0.25));
    let mut scope = f.scope();
    scope.window = Some((0.0, 3.0));
    scope.graph_definition = Some(serde_json::to_value(&edited).unwrap());

    let (manifest, _store) = f.assemble_with(&scope, Some(&contribution)).await;
    assert_eq!(
        reason(&root(&manifest), "graph.run"),
        super::graph::GRAPH_CHANGED
    );
}

#[tokio::test]
async fn moving_a_node_does_not_invalidate_a_run() {
    let f = Fixture::new().await;
    let graph = demo_graph();
    let contribution =
        GraphRunContribution::new(Arc::new(evaluation(&graph, &f.venue_id, (0.0, 3.0))));

    let mut moved = graph.clone();
    moved.nodes[0].position_x = Some(999.0);
    moved.nodes[0].position_y = Some(-42.0);
    let mut scope = f.scope();
    scope.window = Some((0.0, 3.0));
    scope.graph_definition = Some(serde_json::to_value(&moved).unwrap());

    let (manifest, _store) = f.assemble_with(&scope, Some(&contribution)).await;
    assert_eq!(
        at(&root(&manifest), "graph.run.views.view_signal_1")["$kind"],
        "tensor"
    );
}

#[tokio::test]
async fn a_run_from_another_scope_is_never_silently_reused() {
    let f = Fixture::new().await;
    let graph = demo_graph();
    let definition = Some(serde_json::to_value(&graph).unwrap());

    // Different span.
    let contribution =
        GraphRunContribution::new(Arc::new(evaluation(&graph, &f.venue_id, (0.0, 3.0))));
    let mut scope = f.scope();
    scope.graph_definition = definition.clone();
    scope.window = Some((10.0, 20.0));
    let (manifest, _store) = f.assemble_with(&scope, Some(&contribution)).await;
    assert!(reason(&root(&manifest), "graph.run").contains("not the window in scope"));

    // Different venue.
    let contribution =
        GraphRunContribution::new(Arc::new(evaluation(&graph, "some-other-venue", (0.0, 3.0))));
    let mut scope = f.scope();
    scope.graph_definition = definition.clone();
    scope.window = Some((0.0, 3.0));
    let (manifest, _store) = f.assemble_with(&scope, Some(&contribution)).await;
    assert!(reason(&root(&manifest), "graph.run").contains("different venue"));

    // Different track.
    let mut other = evaluation(&graph, &f.venue_id, (0.0, 3.0));
    other.track_id = "trk-other".into();
    let contribution = GraphRunContribution::new(Arc::new(other));
    let mut scope = f.scope();
    scope.graph_definition = definition;
    scope.window = Some((0.0, 3.0));
    let (manifest, _store) = f.assemble_with(&scope, Some(&contribution)).await;
    assert!(reason(&root(&manifest), "graph.run").contains("different track"));
}

#[tokio::test]
async fn an_unknown_agent_kind_is_a_hard_error() {
    let f = Fixture::new().await;
    let mut scope = f.scope();
    scope.agent_kind = "wat".into();
    let mut store = f.store();
    let e = assemble_bindings(
        &f.pool,
        &f.storage,
        &f.resource_root,
        &scope,
        None,
        &mut store,
    )
    .await
    .unwrap_err();
    assert!(e.contains("unknown agent kind"), "{e}");
}
