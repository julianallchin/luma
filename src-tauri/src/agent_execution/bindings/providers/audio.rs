//! `luma.audio` — the mix and the separated stems, as `pcm_f32` artifacts.
//!
//! Nothing is transcoded for the agent: the `.pcm` caches Luma already writes
//! for playback and evaluation are imported as-is and described by a tensor that
//! starts after their 18-byte header. That is why `byte_offset` is 18 and the
//! shape is `[frames, channels]` — interleaved, exactly the file's own layout.
//!
//! The mix is *ensured*: if the cache is missing but the source file is on disk,
//! it is decoded and persisted (the same call playback makes). Stems are not —
//! decoding four stems inline would turn one agent message into a minute of
//! silence, and stem PCM is written by the preprocessing pipeline anyway. A
//! missing stem cache reports the pipeline state instead.

use std::path::{Path, PathBuf};

use super::{missing_reason, unavailable, ProviderCtx, NO_TRACK};
use crate::agent_execution::artifacts::{
    ArtifactEncoding, ArtifactKind, ArtifactStore, ImportRequest,
};
use crate::agent_execution::bindings::assembler::{BindingBuilder, PCM_HEADER_LEN};
use crate::agent_execution::bindings::manifest::{AxisSpec, DType, Provenance, TensorRef};
use crate::database::local;
use crate::services::tracks::TARGET_SAMPLE_RATE;

/// The stems Luma separates, in a fixed order so the manifest is deterministic.
pub const STEM_NAMES: [&str; 4] = ["drums", "bass", "vocals", "other"];

pub async fn provide(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
) -> Result<(), String> {
    let Some(track) = ctx.track.as_ref() else {
        unavailable(b, "audio.mix", NO_TRACK)?;
        for stem in STEM_NAMES {
            unavailable(b, &format!("audio.stems.{stem}"), NO_TRACK)?;
        }
        return Ok(());
    };

    match ensure_mix_pcm(ctx, &track.track_hash, &track.file_path) {
        Ok(path) => bind_pcm(
            b,
            store,
            "audio.mix",
            &path,
            Provenance::new("audio_cache")
                .with_note("full stereo decode, resampled to 48 kHz on import"),
        )?,
        Err(reason) => unavailable(b, "audio.mix", reason)?,
    }

    let stems = local::tracks::get_track_stems(ctx.pool, &track.id)
        .await
        .unwrap_or_default();
    for stem in STEM_NAMES {
        let path = ctx.storage.stem_pcm_path(&track.track_hash, stem);
        if path.exists() {
            bind_pcm(
                b,
                store,
                &format!("audio.stems.{stem}"),
                &path,
                Provenance::new("stem_separation")
                    .with_note(format!("{stem} stem, decoded PCM cache")),
            )?;
            continue;
        }
        let reason = stem_reason(ctx, track, &stems, stem).await;
        unavailable(b, &format!("audio.stems.{stem}"), reason)?;
    }
    Ok(())
}

/// The full-decode PCM cache path, decoding it into place when it is absent and
/// the source audio is still on disk. `Err` is a human-readable reason.
fn ensure_mix_pcm(ctx: &ProviderCtx<'_>, hash: &str, file_path: &str) -> Result<PathBuf, String> {
    if let Some(hit) = existing_mix_cache(ctx, hash, file_path) {
        return Ok(hit);
    }
    let source = Path::new(file_path);
    if !source.exists() {
        return Err(format!(
            "the track's audio file is missing from disk ({file_path}), so no PCM could be decoded"
        ));
    }
    // Writes `<dir of the audio file>/cache/<hash>.pcm` as a side effect — the
    // same cache playback and the evaluator use.
    crate::audio::cache::load_or_decode_audio_shared(source, hash, TARGET_SAMPLE_RATE)
        .map_err(|e| format!("decoding the track's audio failed: {e}"))?;
    existing_mix_cache(ctx, hash, file_path)
        .ok_or_else(|| "the track decoded but no PCM cache file was written".to_string())
}

/// The canonical library location first, then the cache beside the audio file
/// (tracks imported from outside the library live there).
fn existing_mix_cache(ctx: &ProviderCtx<'_>, hash: &str, file_path: &str) -> Option<PathBuf> {
    let library = ctx.storage.mix_pcm_path(hash);
    if library.exists() {
        return Some(library);
    }
    let beside = Path::new(file_path)
        .parent()?
        .join("cache")
        .join(format!("{hash}.pcm"));
    beside.exists().then_some(beside)
}

/// Import a `.pcm` file and bind it as `[frames, channels]` (or `[frames]` when
/// mono) with a linear time axis at its own sample rate.
fn bind_pcm(
    b: &mut BindingBuilder,
    store: &mut ArtifactStore,
    path: &str,
    file: &Path,
    provenance: Provenance,
) -> Result<(), String> {
    let descriptor = store
        .import(ImportRequest::new(
            file,
            ArtifactKind::Tensor,
            ArtifactEncoding::PcmF32,
        ))
        .map_err(String::from)?;

    let sample_rate = descriptor.sample_rate_hz.unwrap_or(TARGET_SAMPLE_RATE);
    let channels = descriptor.channels.unwrap_or(1).max(1) as usize;
    let samples = descriptor.byte_len.saturating_sub(PCM_HEADER_LEN) / 4;
    let frames = samples as usize / channels;

    let time = AxisSpec::linear_unit("time", 0.0, 1.0 / sample_rate as f64, frames, "s");
    let (shape, axes) = if channels == 1 {
        (vec![frames], vec![time])
    } else {
        (
            vec![frames, channels],
            vec![time, AxisSpec::labels("channel", channel_labels(channels))],
        )
    };

    let tensor = TensorRef::new(descriptor.id.clone(), DType::F32, shape, axes, provenance)
        .with_offset(PCM_HEADER_LEN);
    b.artifact(descriptor).map_err(String::from)?;
    b.tensor(path, tensor).map_err(String::from)?;
    Ok(())
}

fn channel_labels(channels: usize) -> Vec<String> {
    if channels == 2 {
        return vec!["l".into(), "r".into()];
    }
    (0..channels).map(|i| format!("ch{i}")).collect()
}

/// Why a stem has no PCM cache: never separated, separated-and-lost, or
/// separated-but-not-yet-decoded. Each is a different thing for the agent to do.
async fn stem_reason(
    ctx: &ProviderCtx<'_>,
    track: &crate::models::tracks::TrackSummary,
    stems: &[crate::models::tracks::TrackStem],
    stem: &str,
) -> String {
    if !stems.iter().any(|s| s.stem_name == stem) {
        return missing_reason(ctx.pool, &track.id, "stems", "stem separation").await;
    }
    match ctx.storage.stem_source_path(&track.track_hash, stem) {
        Some(_) => format!(
            "the {stem} stem is separated but has no decoded PCM cache yet \
             (it is written the first time the stem is played or evaluated)"
        ),
        None => format!(
            "the {stem} stem is recorded in the database but its audio file is missing from disk"
        ),
    }
}
