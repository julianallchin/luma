//! `luma.audio` — the two signals the agent analyses, plus the mix for reference.
//!
//! - `vocals`: the vocals stem.
//! - `rest`: the mix minus the vocals stem — drums, bass and everything else,
//!   so rap harmonics can never pass for a growl.
//! - `mix`: the full mix, for reference.
//!
//! The individual drums/bass/other stems are deliberately not exposed: demucs
//! scatters one sound across them, and an agent that trusts those labels hears
//! the wrong thing. Band splits of `rest` plus n2n drum onsets replace them.
//!
//! Nothing is transcoded for the agent: each signal is a `.pcm` cache imported
//! as-is and described by a tensor that starts after the 18-byte header, so
//! `byte_offset` is 18 and the shape is `[frames, channels]`. All three are
//! *ensured*: mix and vocals go through the one decode cache whenever their
//! source is on disk, which rewrites a stale cache at [`SAMPLE_RATE`], and rest
//! is derived from them and persisted.

use std::path::{Path, PathBuf};

use super::{missing_reason, unavailable, ProviderCtx, NO_TRACK};
use crate::agent_execution::artifacts::{
    ArtifactEncoding, ArtifactKind, ArtifactStore, ImportRequest,
};
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::agent_execution::bindings::manifest::{AxisSpec, DType, Provenance, TensorRef};
use crate::audio::cache::{load_or_decode_audio_shared, read_pcm_file, write_pcm_file};
use crate::audio::{PCM_HEADER_LEN, SAMPLE_RATE};
use crate::database::local;

/// The derived "stem" name of the mix minus vocals, cached beside the stems.
const REST: &str = "rest";

const NO_CACHE_WRITTEN: &str = "the audio decoded but no PCM cache file was written";

pub async fn provide(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
) -> Result<(), String> {
    let Some(track) = ctx.track.as_ref() else {
        for path in ["audio.mix", "audio.vocals", "audio.rest"] {
            unavailable(b, path, NO_TRACK)?;
        }
        return Ok(());
    };

    let mix = ensure_mix_pcm(ctx, &track.track_hash, &track.file_path);
    let vocals = ensure_vocals_pcm(ctx, track).await;
    let rest = match (&mix, &vocals) {
        (Ok(mix), Ok(vocals)) => ensure_rest_pcm(
            mix,
            vocals,
            &ctx.storage.stem_pcm_path(&track.track_hash, REST),
        ),
        (Err(reason), _) | (_, Err(reason)) => {
            Err(format!("rest is the mix minus vocals: {reason}"))
        }
    };

    for (path, signal, note) in [
        (
            "audio.mix",
            mix,
            "full stereo mix at 48 kHz, for reference; analyse rest and vocals",
        ),
        ("audio.vocals", vocals, "vocals stem, decoded PCM cache"),
        (
            "audio.rest",
            rest,
            "mix minus the vocals stem: drums, bass and everything else",
        ),
    ] {
        match signal {
            Ok(file) => bind_pcm(
                b,
                store,
                path,
                &file,
                Provenance::new("audio_cache").with_note(note),
            )?,
            Err(reason) => unavailable(b, path, reason)?,
        }
    }
    Ok(())
}

/// The vocals stem's PCM cache, decoded through the one decode cache when the
/// separated stem is on disk. `Err` says whether separation never ran or its
/// file is gone.
async fn ensure_vocals_pcm(
    ctx: &ProviderCtx<'_>,
    track: &crate::models::tracks::TrackSummary,
) -> Result<PathBuf, String> {
    let cache = ctx.storage.stem_pcm_path(&track.track_hash, "vocals");
    if let Some(source) = ctx.storage.stem_source_path(&track.track_hash, "vocals") {
        // Keyed so the decode cache lands exactly on `stem_pcm_path`.
        let key = format!("{}_stem_vocals", track.track_hash);
        load_or_decode_audio_shared(&source, &key)
            .map_err(|e| format!("decoding the vocals stem failed: {e}"))?;
        return cache
            .exists()
            .then_some(cache)
            .ok_or_else(|| NO_CACHE_WRITTEN.into());
    }
    if cache.exists() {
        return Ok(cache);
    }
    let stems = local::tracks::get_track_stems(ctx.pool, &track.id)
        .await
        .unwrap_or_default();
    if !stems.iter().any(|s| s.stem_name == "vocals") {
        return Err(missing_reason(ctx.pool, &track.id, "stems", "stem separation").await);
    }
    Err(
        "the vocals stem is recorded in the database but its audio file is missing from disk"
            .into(),
    )
}

/// `mix - vocals`, written once and reused while it is newer than both inputs.
fn ensure_rest_pcm(mix: &Path, vocals: &Path, rest: &Path) -> Result<PathBuf, String> {
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if let (Some(done), Some(a), Some(b)) = (modified(rest), modified(mix), modified(vocals)) {
        if done >= a && done >= b {
            return Ok(rest.to_path_buf());
        }
    }
    let mix = read_pcm_file(mix)?;
    let vocals = read_pcm_file(vocals)?;
    // Invariant, not a fallback: every decode cache is stereo at SAMPLE_RATE.
    if (mix.sample_rate, mix.channels) != (vocals.sample_rate, vocals.channels) {
        return Err(format!(
            "the mix ({} Hz, {} ch) and vocals ({} Hz, {} ch) caches do not line up",
            mix.sample_rate, mix.channels, vocals.sample_rate, vocals.channels
        ));
    }
    let samples: Vec<f32> = mix
        .samples
        .iter()
        .zip(&vocals.samples)
        .map(|(m, v)| m - v)
        .collect();
    write_pcm_file(rest, &samples, mix.sample_rate, mix.channels)?;
    Ok(rest.to_path_buf())
}

/// The full-decode PCM cache path, decoded through the one decode cache while the
/// source audio is on disk. `Err` is a human-readable reason.
fn ensure_mix_pcm(ctx: &ProviderCtx<'_>, hash: &str, file_path: &str) -> Result<PathBuf, String> {
    let source = Path::new(file_path);
    if !source.exists() {
        return existing_mix_cache(ctx, hash, file_path).ok_or_else(|| {
            format!(
                "the track's audio file is missing from disk ({file_path}), so no PCM could be decoded"
            )
        });
    }
    // Writes `<dir of the audio file>/cache/<hash>.pcm` as a side effect — the
    // same cache playback and the evaluator use.
    load_or_decode_audio_shared(source, hash)
        .map_err(|e| format!("decoding the track's audio failed: {e}"))?;
    existing_mix_cache(ctx, hash, file_path).ok_or_else(|| NO_CACHE_WRITTEN.into())
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

    let sample_rate = descriptor.sample_rate_hz.unwrap_or(SAMPLE_RATE);
    let channels = descriptor.channels.unwrap_or(1).max(1) as usize;
    let samples = descriptor.byte_len.saturating_sub(PCM_HEADER_LEN as u64) / 4;
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
        .with_offset(PCM_HEADER_LEN as u64);
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
