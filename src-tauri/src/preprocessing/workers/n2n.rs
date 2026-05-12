//! n2n drum-onset preprocessor.
//!
//! Runs the diffusion-based ADT model from `julianallchin/n2n` (a
//! paper-aligned reproduction of Yeung et al., Sony AI 2025) on full-mix
//! audio + the shared MERT cache produced by [`super::mert`]. Both
//! conditioning streams (mel + MERT) come from the same full-mix track so
//! they describe the same audio.
//!
//! Output is a JSON blob `{class_name: [t, ...]}` keyed by the model's native
//! 4-class taxonomy (kick / snare / hat / cymbal — toms intentionally
//! dropped, ride merged into cymbal). Predecessor was the ADTOF Frame_RNN
//! head (v1, MIDI keys + 5 classes including tom).
//!
//! ⚠ Distribution shift: v6+ checkpoints were trained on drum-isolated stems.
//! Running on full-mix audio is faster (no demucs gate) and consistent with
//! the classifier's MERT cache, but moves both conditioning streams off the
//! trained input distribution. The bundled v12 checkpoint (run012 step 42000)
//! adds an ADTOF full-mix dataset component which closes most of that gap.
//! v4 bumps `version()` to invalidate v3 rows computed with the older v10
//! diffusion checkpoint, so every existing track reprocesses on next launch.
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
        7
    }
    fn inputs(&self) -> &'static [Artifact] {
        // No Stems dependency — n2n now runs on the full mix, the same audio
        // the classifier and MERT cache see.
        &[Artifact::Mert]
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
        let audio_path: std::path::PathBuf = track.file_path.clone().into();
        let mert_path = tracks_db::get_track_mert_path(ctx.pool(), track_id)
            .await?
            .ok_or_else(|| format!("Missing MERT cache row for track {track_id}"))?;
        let mert_path: std::path::PathBuf = mert_path.into();
        let handle = ctx.app_handle().clone();

        let onsets = tauri::async_runtime::spawn_blocking(move || {
            n2n_worker::compute_drum_onsets(&handle, Path::new(&audio_path), Path::new(&mert_path))
        })
        .await
        .map_err(|e| format!("n2n worker task failed: {e}"))??;

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
