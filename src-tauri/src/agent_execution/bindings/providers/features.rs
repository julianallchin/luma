//! `luma.features` — everything Luma's analysis pipeline knows about the music.
//!
//! Every branch here answers "absent, failed, or empty?" the same way (report
//! §7e): a missing row plus a `preprocessing_failures` row reports the worker's
//! own error; a missing row with no failure reports "has not run"; a present row
//! produces a tensor, even a zero-length one. An empty tensor means the analysis
//! ran and found nothing — that is real information and must not be flattened
//! into "unavailable".

use std::collections::BTreeMap;

use serde_json::Value;

use super::{
    inline, missing_reason, put_event_times, put_f32, put_f64, put_i64, unavailable, ProviderCtx,
    NO_TRACK,
};
use crate::agent_execution::artifacts::{
    ArtifactEncoding, ArtifactKind, ArtifactStore, ImportRequest,
};
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::agent_execution::bindings::manifest::{AxisSpec, Provenance, TensorRef};
use crate::classifier_worker::BarClassification;
use crate::database::local;

/// Drum-onset classes the n2n worker emits.
pub const DRUM_CLASSES: [&str; 4] = ["kick", "snare", "hat", "cymbal"];

/// The one continuous regression head hiding among the classifier's sigmoid
/// tags. It must be split out: averaging it with probabilities is nonsense.
const INTENSITY_KEY: &str = "intensity";

/// MERT-95M layer 7 runs at 75 frames per second.
const MERT_FRAME_RATE_HZ: f64 = 75.0;

/// `hat` (drum onsets) and `hats` (bar tags) are different vocabularies from
/// different models. Renaming either would break stored data, so the mismatch is
/// documented where the agent will actually read it.
const DRUM_CLASS_NOTE: &str = "onset classes are kick/snare/hat/cymbal; \
    the bar classifier's hi-hat tag is spelled 'hats' — different models, different vocabularies";

/// The stored mel spectrogram is a display asset, not an analysis product.
pub const MEL_UNAVAILABLE: &str = "not exposed: Luma's stored mel spectrogram is \
    display-normalized (log-scaled then min-max scaled to 0-1) and resampled to a \
    fixed 512 columns, which is misleading for analysis — compute one from \
    luma.audio.mix with librosa instead";

pub async fn provide(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
) -> Result<(), String> {
    let Some(track) = ctx.track.as_ref() else {
        for path in [
            "features.beats",
            "features.downbeats",
            "features.bpm",
            "features.beats_per_bar",
            "features.drum_onsets",
            "features.bars",
            "features.chords",
            "features.waveform_bands",
            "features.mel",
            "features.mert",
        ] {
            unavailable(b, path, NO_TRACK)?;
        }
        return Ok(());
    };
    let track_id = track.id.as_str();

    beats(b, ctx, store, track_id).await?;
    drum_onsets(b, ctx, store, track_id).await?;
    bars(b, ctx, store, track_id).await?;
    chords(b, ctx, store, track_id).await?;
    waveform_bands(b, ctx, store, track_id).await?;
    unavailable(b, "features.mel", MEL_UNAVAILABLE)?;
    mert(b, ctx, store, track_id).await
}

// ---------------------------------------------------------------------------
// Beat grid
// ---------------------------------------------------------------------------

async fn beats(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
    track_id: &str,
) -> Result<(), String> {
    let row = local::tracks::get_track_beats_raw(ctx.pool, track_id).await?;
    let Some(row) = row else {
        let reason = missing_reason(ctx.pool, track_id, "beat_grid", "beat detection").await;
        for path in [
            "features.beats",
            "features.downbeats",
            "features.bpm",
            "features.beats_per_bar",
        ] {
            unavailable(b, path, reason.clone())?;
        }
        return Ok(());
    };

    let provenance = Provenance::new("beat_this").with_version(version_of("track_beats"));
    let beats: Vec<f32> = serde_json::from_str(&row.beats_json).unwrap_or_default();
    let downbeats: Vec<f32> = serde_json::from_str(&row.downbeats_json).unwrap_or_default();
    put_event_times(b, store, "features.beats", &beats, provenance.clone())?;
    put_event_times(
        b,
        store,
        "features.downbeats",
        &downbeats,
        provenance.with_note("first beat of each bar"),
    )?;
    inline(b, "features.bpm", row.bpm)?;
    inline(b, "features.beats_per_bar", row.beats_per_bar)?;
    Ok(())
}

/// Current processor version for an artifact table, as a string for provenance.
fn version_of(artifact_table: &str) -> String {
    crate::preprocessing::registry::current_artifact_versions()
        .get(artifact_table)
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".into())
}

// ---------------------------------------------------------------------------
// Drum onsets
// ---------------------------------------------------------------------------

async fn drum_onsets(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
    track_id: &str,
) -> Result<(), String> {
    let onsets = local::tracks::get_track_drum_onsets(ctx.pool, track_id).await?;
    let Some(onsets) = onsets else {
        let reason = missing_reason(ctx.pool, track_id, "n2n", "drum-onset detection").await;
        return unavailable(b, "features.drum_onsets", reason);
    };

    let provenance = Provenance::new("n2n")
        .with_version(version_of("track_drum_onsets"))
        .with_note(DRUM_CLASS_NOTE);
    // Canonical classes first, then anything else the model produced, so a new
    // class shows up without a code change.
    let extra: Vec<&String> = onsets
        .keys()
        .filter(|k| !DRUM_CLASSES.contains(&k.as_str()))
        .collect();
    for class in DRUM_CLASSES.iter().map(|c| c.to_string()).chain(
        extra
            .into_iter()
            .filter(|k| is_safe_record_key(k))
            .cloned()
            .collect::<Vec<_>>(),
    ) {
        let path = format!("features.drum_onsets.{class}");
        match onsets.get(&class) {
            Some(times) => put_event_times(b, store, &path, times, provenance.clone())?,
            None => unavailable(
                b,
                &path,
                format!("the drum-onset model did not emit a '{class}' class for this track"),
            )?,
        }
    }
    Ok(())
}

/// Record keys the Python binding object cannot expose as attributes without
/// shadowing its own mapping API (appendix A.11).
pub(crate) fn is_safe_record_key(key: &str) -> bool {
    !matches!(key, "keys" | "items" | "values" | "get")
        && !key.is_empty()
        && !key.contains('.')
        && !key.contains('$')
        && !key.chars().any(|c| c.is_whitespace())
}

// ---------------------------------------------------------------------------
// Bar classifications
// ---------------------------------------------------------------------------

async fn bars(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
    track_id: &str,
) -> Result<(), String> {
    let row = local::tracks::get_track_bar_classifications_raw(ctx.pool, track_id).await?;
    let Some((classifications_json, tag_order_json)) = row else {
        let reason = missing_reason(ctx.pool, track_id, "classifier", "bar classification").await;
        return unavailable(b, "features.bars", reason);
    };
    let parsed: Vec<BarClassification> = match serde_json::from_str(&classifications_json) {
        Ok(v) => v,
        Err(e) => {
            return unavailable(
                b,
                "features.bars",
                format!("the stored bar classifications could not be parsed: {e}"),
            )
        }
    };
    let tags: Vec<String> = serde_json::from_str(&tag_order_json).unwrap_or_default();

    let n_bars = parsed.len();
    let indices: Vec<i64> = parsed.iter().map(|bar| bar.bar_idx as i64).collect();
    let starts: Vec<f64> = parsed.iter().map(|bar| bar.start).collect();
    let ends: Vec<f64> = parsed.iter().map(|bar| bar.end).collect();
    let intensity: Vec<f64> = parsed
        .iter()
        .map(|bar| {
            bar.predictions
                .get(INTENSITY_KEY)
                .copied()
                .unwrap_or(f64::NAN)
        })
        .collect();

    let version = version_of("track_bar_classifications");
    let provenance = Provenance::new("bar_window_classifier").with_version(version);
    let bar_axis = || AxisSpec::coordinates("bar", starts.clone(), Some("s".into()));

    put_i64(
        b,
        store,
        "features.bars.indices",
        &indices,
        vec![AxisSpec::index("bar", n_bars)],
        provenance.clone(),
    )?;
    put_f64(
        b,
        store,
        "features.bars.starts_s",
        &starts,
        vec![AxisSpec::index("bar", n_bars)],
        Some("s"),
        provenance.clone(),
    )?;
    put_f64(
        b,
        store,
        "features.bars.ends_s",
        &ends,
        vec![AxisSpec::index("bar", n_bars)],
        Some("s"),
        provenance.clone(),
    )?;
    put_f64(
        b,
        store,
        "features.bars.intensity",
        &intensity,
        vec![bar_axis()],
        None,
        provenance.clone().with_note(
            "continuous regression head clipped to 0..5 — NOT a probability, and \
             deliberately split out of `predictions`",
        ),
    )?;

    // [bar, tag] in tag_order. NaN marks a tag the classifier did not emit for
    // that bar, which is distinguishable from a confident zero.
    let mut predictions = Vec::with_capacity(n_bars * tags.len());
    for bar in &parsed {
        for tag in &tags {
            predictions.push(
                bar.predictions
                    .get(tag)
                    .copied()
                    .map(|v| v as f32)
                    .unwrap_or(f32::NAN),
            );
        }
    }
    put_f32(
        b,
        store,
        "features.bars.predictions",
        &predictions,
        vec![bar_axis(), AxisSpec::labels("tag", tags.clone())],
        None,
        provenance.with_note(
            "per-tag sigmoid probabilities; compare against features.bars.thresholds, \
             not a flat 0.5",
        ),
    )?;
    inline(b, "features.bars.tags", &tags)?;

    match bundled_thresholds() {
        Ok(thresholds) => inline(b, "features.bars.thresholds", &thresholds)?,
        Err(e) => unavailable(b, "features.bars.thresholds", e)?,
    }
    Ok(())
}

/// F1-optimal per-tag thresholds bundled with the classifier weights. Flat 0.5
/// badly over-suppresses rare tags (`vocal_chop` is 0.165).
fn bundled_thresholds() -> Result<BTreeMap<String, f64>, String> {
    let raw: Value = serde_json::from_str(crate::classifier_worker::bundled_thresholds())
        .map_err(|e| format!("the bundled classifier thresholds could not be parsed: {e}"))?;
    let map = raw
        .get("thresholds")
        .and_then(|v| v.as_object())
        .ok_or_else(|| {
            "the bundled classifier thresholds have no `thresholds` object".to_string()
        })?;
    Ok(map
        .iter()
        .filter(|(k, _)| is_safe_record_key(k))
        .filter_map(|(k, v)| v.as_f64().map(|v| (k.clone(), v)))
        .collect())
}

// ---------------------------------------------------------------------------
// Chords
// ---------------------------------------------------------------------------

async fn chords(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
    track_id: &str,
) -> Result<(), String> {
    let sections = local::tracks::get_track_chord_sections(ctx.pool, track_id).await?;
    let Some(sections) = sections else {
        let reason = missing_reason(ctx.pool, track_id, "roots", "chord/root detection").await;
        return unavailable(b, "features.chords", reason);
    };

    let n = sections.len();
    let starts: Vec<f64> = sections.iter().map(|s| s.start_s as f64).collect();
    let ends: Vec<f64> = sections.iter().map(|s| s.end_s as f64).collect();
    // A section can genuinely have no root (silence, "N", low confidence). NaN
    // carries that in a float tensor; -1 would be a sentinel the agent has to
    // remember, and 0 would be a lie (0 is C).
    let roots: Vec<f64> = sections
        .iter()
        .map(|s| s.root_pitch_class.map_or(f64::NAN, |r| r as f64))
        .collect();
    let labels: Vec<Option<String>> = sections.iter().map(|s| s.label.clone()).collect();

    let provenance = Provenance::new("consonance_ace").with_version(version_of("track_roots"));
    let section_axis = || AxisSpec::index("section", n);
    put_f64(
        b,
        store,
        "features.chords.starts_s",
        &starts,
        vec![section_axis()],
        Some("s"),
        provenance.clone(),
    )?;
    put_f64(
        b,
        store,
        "features.chords.ends_s",
        &ends,
        vec![section_axis()],
        Some("s"),
        provenance.clone(),
    )?;
    put_f64(
        b,
        store,
        "features.chords.root_pitch_class",
        &roots,
        vec![section_axis()],
        None,
        provenance.with_note(
            "pitch class 0-11 (0 = C); NaN where the section has no detected root — \
             see features.chords.labels for the full chord symbol",
        ),
    )?;
    inline(b, "features.chords.labels", &labels)
}

// ---------------------------------------------------------------------------
// Waveform bands
// ---------------------------------------------------------------------------

async fn waveform_bands(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
    track_id: &str,
) -> Result<(), String> {
    let row = local::waveforms::fetch_track_waveform(ctx.pool, track_id).await?;
    let Some(waveform) = row else {
        return unavailable(
            b,
            "features.waveform_bands",
            "waveform analysis has not run for this track",
        );
    };
    let Some(bands) = waveform.bands else {
        return unavailable(
            b,
            "features.waveform_bands",
            "this track's waveform predates 3-band envelopes and only has min/max samples",
        );
    };
    let n = bands.low.len();
    if bands.mid.len() != n || bands.high.len() != n {
        return unavailable(
            b,
            "features.waveform_bands",
            "the stored band envelopes have inconsistent lengths",
        );
    }

    // Buckets tile the *decoded* audio uniformly, so bucket i is centered at
    // (i + 0.5) * duration / n — exactly a linear axis. `decoded_duration` is the
    // true length; tracks.duration_seconds is metadata and can disagree.
    let duration = if waveform.duration_seconds > 0.0 {
        waveform.duration_seconds
    } else {
        ctx.track
            .as_ref()
            .and_then(|t| t.duration_seconds)
            .unwrap_or(0.0)
    };
    let step = if n > 0 { duration / n as f64 } else { 0.0 };

    let mut data = Vec::with_capacity(n * 3);
    data.extend_from_slice(&bands.low);
    data.extend_from_slice(&bands.mid);
    data.extend_from_slice(&bands.high);
    put_f32(
        b,
        store,
        "features.waveform_bands",
        &data,
        vec![
            AxisSpec::labels("band", vec!["low".into(), "mid".into(), "high".into()]),
            AxisSpec::linear_unit("time", step / 2.0, step, n, "s"),
        ],
        None,
        Provenance::new("waveform_bands").with_note(
            "0-1 normalized band energy over the full-resolution bucket grid; \
             bucket times are derived from the decoded audio duration",
        ),
    )
}

// ---------------------------------------------------------------------------
// MERT
// ---------------------------------------------------------------------------

async fn mert(
    b: &mut BindingBuilder,
    ctx: &ProviderCtx<'_>,
    store: &mut ArtifactStore,
    track_id: &str,
) -> Result<(), String> {
    let paths = local::tracks::get_track_mert_paths(ctx.pool, track_id).await?;
    let Some((fullmix, drum)) = paths else {
        let reason = missing_reason(ctx.pool, track_id, "mert", "MERT feature extraction").await;
        return unavailable(b, "features.mert", reason);
    };
    let hash = ctx.track_hash().unwrap_or_default();

    for (key, recorded, fallback, note) in [
        (
            "fullmix",
            fullmix,
            ctx.storage.mert_fullmix_path(hash),
            "MERT-95M layer 7 over the full mix",
        ),
        (
            "drum",
            drum,
            ctx.storage.mert_drum_path(hash),
            "MERT-95M layer 7 over the isolated drum stem",
        ),
    ] {
        let path = format!("features.mert.{key}");
        let file = std::path::PathBuf::from(&recorded);
        let file = if file.exists() { file } else { fallback };
        if !file.exists() {
            unavailable(
                b,
                &path,
                format!(
                    "the {key} MERT cache is recorded in the database but the file is missing \
                     from disk ({recorded})"
                ),
            )?;
            continue;
        }
        match bind_npy(b, store, &path, &file, note) {
            Ok(()) => {}
            Err(e) => unavailable(b, &path, format!("the {key} MERT cache is unreadable: {e}"))?,
        }
    }
    Ok(())
}

/// Import a `.npy` untouched and describe it from its own header — the shape and
/// dtype in the manifest are read back off the file, never assumed.
fn bind_npy(
    b: &mut BindingBuilder,
    store: &mut ArtifactStore,
    path: &str,
    file: &std::path::Path,
    note: &str,
) -> Result<(), String> {
    let header = crate::agent_execution::artifacts::codecs::read_npy_header(file)?;
    if header.shape.len() != 2 {
        return Err(format!(
            "expected a 2-D [frame, feature] array, got shape {:?}",
            header.shape
        ));
    }
    let descriptor = store.import(ImportRequest::new(
        file,
        ArtifactKind::Tensor,
        ArtifactEncoding::Npy,
    ))?;
    let (frames, features) = (header.shape[0], header.shape[1]);
    let tensor = TensorRef::new(
        descriptor.id.clone(),
        header.dtype,
        vec![frames, features],
        vec![
            AxisSpec::linear_unit("time", 0.0, 1.0 / MERT_FRAME_RATE_HZ, frames, "s"),
            AxisSpec::index("feature", features),
        ],
        Provenance::new("mert")
            .with_version(version_of("track_mert"))
            .with_note(format!("{note}, 75 frames/s")),
    );
    b.artifact(descriptor).map_err(String::from)?;
    b.tensor(path, tensor).map_err(String::from)?;
    Ok(())
}
