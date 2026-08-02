//! n2n drum-onset preprocessor.
//!
//! Runs the ADT model from `julianallchin/n2n` (a paper-aligned reproduction
//! of Yeung et al., Sony AI 2025) on the demucs **drum stem**: both
//! conditioning streams (log-mel + MERT) are derived from `drums.ogg`. The
//! drum MERT cache is produced by [`super::mert`] in the same Python process
//! that builds the classifier's full-mix cache, so we still pay one MERT
//! model load per track.
//!
//! Output is a JSON blob `{class_name: [t, ...]}` keyed by the model's
//! native 4-class taxonomy (kick / snare / hat / cymbal — toms intentionally
//! dropped, ride merged into cymbal). Predecessor was the ADTOF Frame_RNN
//! head (v1, MIDI keys + 5 classes including tom).
//!
//! v6+ checkpoints were trained on drum-isolated stems, so both streams
//! must come from `drums.ogg` to stay on the trained input distribution.
//! v3–v8 ran inference on the full mix (sharing the classifier's MERT
//! cache); we moved back to drum-stem inputs once we saw the distribution
//! gap show up in produced onsets on real tracks.
//!
//! Model weights (~190 MB EMA + config, fp32) ship inline at
//! `python/n2n/weights.pt`; the vendored `python/n2n/` package contains the
//! sampler / decoder / log-mel frontend. `python_env.rs` installs python
//! deps via `python/n2n/requirements.txt`.

use std::path::Path;

use async_trait::async_trait;

use crate::database::local::tracks as tracks_db;
use crate::n2n_worker;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext};

pub struct N2NPreprocessor;

#[async_trait]
impl Preprocessor for N2NPreprocessor {
    fn name(&self) -> &'static str {
        "n2n"
    }
    fn version(&self) -> u32 {
        // v1: ADTOF Frame_RNN, 5-MIDI keys.
        // v2: n2n v10 on drum stems, 4-class names.
        // v3: n2n v10 on full-mix audio + shared MERT cache.
        // v4: n2n v12 (BCE sigmoid head, no diffusion) run012 step 42000,
        //     peak-pick threshold 0.9 calibrated against ADTOF F1.
        // v5–v6: local drum-detector experiments (never released).
        // v7: floor past local experiment rows on dev branches.
        // v8: n2n v12 run012 step 136000 — training plateau. Final sweep
        //     peaks: EGMD F1 0.891 @ thr=0.93, ADTOF F1 0.899 @ thr=0.95.
        //     +0.049 over v11/run011 on deployment metric. PARTY 4 U hat
        //     count climbed 17 → 89 vs step 42000.
        // v9: back to drum-stem inputs for both log-mel and MERT — v6+
        //     checkpoints were trained on drum-isolated stems and the
        //     full-mix distribution shift was visible in onsets.
        // v10: paired with mert v3 (training-aligned 30/15/3 chunking). v9
        //      consumed mert v2's 60/30/5 cache and lost almost every hat on
        //      real tracks; bump invalidates onset rows so reconcile reruns.
        10
    }
    fn inputs(&self) -> &'static [Artifact] {
        // Stems for drums.ogg (log-mel input); Mert for the drum MERT cache
        // emitted alongside the full-mix cache by the `mert` preprocessor.
        &[Artifact::Stems, Artifact::Mert]
    }
    fn output(&self) -> Artifact {
        Artifact::DrumOnsets
    }
    fn status_label(&self) -> &'static str {
        "Transcribing drums…"
    }
    fn artifact_table(&self) -> &'static str {
        "track_drum_onsets"
    }

    async fn run(&self, ctx: &PreprocessorContext<'_>, track_id: &str) -> Result<(), String> {
        let track = ctx.track();
        let stems_dir = ctx.stems_dir().join(&track.track_hash);
        let drum_audio = crate::preprocessing::workers::stems::find_stem_file(&stems_dir, "drums")
            .ok_or_else(|| {
                format!(
                    "Missing drums stem for track {track_id} under {}",
                    stems_dir.display()
                )
            })?;
        let (_fullmix_path, drum_mert_path) = tracks_db::get_track_mert_paths(ctx.pool(), track_id)
            .await?
            .ok_or_else(|| format!("Missing MERT cache row for track {track_id}"))?;
        if drum_mert_path.is_empty() {
            return Err(format!(
                "Drum MERT cache path missing for track {track_id} \
                 (legacy v1 mert row — bump mert.version() to refresh)"
            ));
        }
        let drum_mert_path: std::path::PathBuf = drum_mert_path.into();
        let handle = ctx.app_handle().clone();

        let onsets = tauri::async_runtime::spawn_blocking(move || {
            n2n_worker::compute_drum_onsets(
                &handle,
                Path::new(&drum_audio),
                Path::new(&drum_mert_path),
            )
        })
        .await
        .map_err(|e| format!("n2n worker task failed: {e}"))??;
        ctx.checkpoint()?;

        let onsets_json = serde_json::to_string(&onsets.onsets)
            .map_err(|e| format!("Failed to serialize drum onsets: {e}"))?;

        tracks_db::upsert_track_drum_onsets(ctx.pool(), track_id, &onsets_json, self.version())
            .await
    }
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;

    use super::N2NPreprocessor;
    use crate::preprocessing::preprocessor::Preprocessor;
    use crate::preprocessing::registry;
    use crate::preprocessing::scheduler::topo_layers;

    /// Spin up an in-memory pool with just enough schema to back the
    /// `is_complete` / `list_pending` defaults.
    async fn test_pool() -> SqlitePool {
        let opts = SqliteConnectOptions::new()
            .filename(":memory:")
            .create_if_missing(true)
            .foreign_keys(false);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .expect("in-memory db");

        sqlx::query(
            "CREATE TABLE tracks (
                id TEXT PRIMARY KEY,
                file_path TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE track_drum_onsets (
                track_id TEXT PRIMARY KEY,
                onsets_json TEXT NOT NULL,
                processor_version INTEGER NOT NULL DEFAULT 1
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE preprocessing_failures (
                track_id TEXT NOT NULL,
                preprocessor TEXT NOT NULL,
                version INTEGER NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 1,
                last_error TEXT NOT NULL,
                last_attempt TEXT NOT NULL,
                next_retry_at TEXT NOT NULL,
                PRIMARY KEY (track_id, preprocessor)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn list_pending_returns_tracks_without_onsets() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();

        let p = N2NPreprocessor;
        let pending = p.list_pending(&pool).await.unwrap();
        assert_eq!(pending, vec!["t1".to_string()]);

        // Insert a current-version row → no longer pending.
        let v = p.version() as i64;
        sqlx::query(
            "INSERT INTO track_drum_onsets (track_id, onsets_json, processor_version)
             VALUES ('t1', '{}', ?)",
        )
        .bind(v)
        .execute(&pool)
        .await
        .unwrap();
        let pending = p.list_pending(&pool).await.unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn stale_adtof_rows_are_repreprocessed() {
        // Rows persisted by the v1 ADTOF preprocessor have `processor_version =
        // 1`; the n2n preprocessor (version 2) must consider them stale so
        // existing libraries automatically re-run drum transcription on launch.
        let pool = test_pool().await;
        sqlx::query("INSERT INTO tracks (id, file_path) VALUES ('t1', '/audio/t1.mp3')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track_drum_onsets (track_id, onsets_json, processor_version)
             VALUES ('t1', '{}', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let p = N2NPreprocessor;
        let pending = p.list_pending(&pool).await.unwrap();
        assert_eq!(pending, vec!["t1".to_string()]);
    }

    #[test]
    fn n2n_lands_after_mert() {
        // n2n declares Mert as an input, so the scheduler must place it in a
        // strictly later topo layer than `mert`. (Pre-v3 it landed alongside
        // `roots` because both depended on `Stems`; the dependency moved.)
        let layered = topo_layers(&registry::registered_preprocessors());
        let layer_of = |name: &str| -> Option<usize> {
            layered
                .layers()
                .iter()
                .position(|layer| layer.iter().any(|p| p.name() == name))
        };
        let mert_layer = layer_of("mert").expect("mert in registry");
        let n2n_layer = layer_of("n2n").expect("n2n in registry");
        assert!(
            mert_layer < n2n_layer,
            "mert layer {mert_layer} must precede n2n layer {n2n_layer}",
        );
    }
}
