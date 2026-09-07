//! Track data for canonical graphs. Load only requested sources under the
//! score's authorized snapshot; all frame sampling is pure and seekable.
use crate::{database::local::venue_access::AuthorizedVenue, storage::StorageRoot};
use luma_patterns::{self as p, FeatureRequest, FeatureSample};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug)]
pub(crate) struct TrackFeatures {
    clock: p::BeatTimeline,
    audio: BTreeMap<p::AudioSource, super::ResidentAudio>,
    onsets: BTreeMap<p::Drum, Vec<f64>>,
    harmony: Option<Vec<(f64, f64, Option<u8>)>>,
}
impl p::FeatureSource for TrackFeatures {
    fn sample(&self, request: &FeatureRequest, beat: f64) -> p::Result<FeatureSample> {
        match request {
            FeatureRequest::Band {
                source,
                low_hz,
                high_hz,
            } => {
                let audio = self
                    .audio
                    .get(source)
                    .ok_or_else(|| p::Error(format!("{} audio was not prepared", source.name())))?;
                let seconds = self.clock.seconds_at(beat)?;
                Ok(FeatureSample::Energy(f64::from(
                    super::ops::audio::sample_band(
                        audio,
                        seconds as f32,
                        [*low_hz as f32, *high_hz as f32],
                    ),
                )))
            }
            FeatureRequest::Onsets(drum) => {
                let events = self
                    .onsets
                    .get(drum)
                    .ok_or_else(|| p::Error(format!("{} onsets were not prepared", drum.name())))?;
                let next = events.partition_point(|at| *at <= beat);
                Ok(FeatureSample::Onset(
                    next.checked_sub(1)
                        .map(|index| (events[index], index as u64)),
                ))
            }
            FeatureRequest::Harmony => {
                let sections = self
                    .harmony
                    .as_ref()
                    .ok_or_else(|| p::Error("harmony was not prepared".into()))?;
                Ok(FeatureSample::PitchClass(
                    sections
                        .iter()
                        .rev()
                        .find(|(start, end, _)| beat >= *start && beat < *end)
                        .and_then(|(_, _, pitch)| *pitch),
                ))
            }
        }
    }
}

pub(crate) async fn prepare(
    access: &mut impl AuthorizedVenue,
    storage: &StorageRoot,
    track_id: &str,
    clock: p::BeatTimeline,
    requests: &[FeatureRequest],
) -> Result<Arc<dyn p::FeatureSource>, String> {
    let mut result = TrackFeatures {
        clock,
        audio: BTreeMap::new(),
        onsets: BTreeMap::new(),
        harmony: None,
    };
    let needs_audio = requests
        .iter()
        .any(|r| matches!(r, FeatureRequest::Band { .. }));
    if needs_audio {
        let path = crate::database::local::tracks::get_track_path_and_hash_for_connection(
            access.connection(),
            track_id,
        )
        .await?;
        for request in requests {
            let FeatureRequest::Band {
                source, high_hz, ..
            } = request
            else {
                continue;
            };
            if !result.audio.contains_key(source) {
                let audio = if *source == p::AudioSource::Mix {
                    super::context::load_track_audio_cached(
                        storage,
                        &path.file_path,
                        &path.track_hash,
                    )?
                } else {
                    let file = storage.stem_pcm_path(&path.track_hash, source.name());
                    let decoded = match crate::audio::read_pcm_file(&file) {
                        Ok(pcm) => Arc::new(crate::audio::decoder::DecodedAudio::from(pcm)),
                        Err(cache_error) => {
                            let source_file = storage.stem_source_path(&path.track_hash, source.name())
                                .ok_or_else(|| format!("{} stem unavailable; analyze the track's stems: {cache_error}", source.name()))?;
                            // Compressed stems sync; decoded PCM is a local cache.
                            // Rebuild it from this exact source, never the full mix.
                            crate::audio::load_or_decode_audio_shared(
                                &source_file,
                                &format!("{}_stem_{}", path.track_hash, source.name()),
                                0,
                            )
                            .map_err(|e| format!("could not decode {} stem: {e}", source.name()))?
                        }
                    };
                    if decoded.channels != 1 && decoded.channels != 2 {
                        return Err(format!(
                            "{} stem has unsupported channel count {}",
                            source.name(),
                            decoded.channels
                        ));
                    }
                    let mono = if decoded.channels == 2 {
                        crate::audio::stereo_to_mono(&decoded.samples)
                    } else {
                        decoded.samples.clone()
                    };
                    super::ResidentAudio {
                        samples: Arc::new(mono),
                        sample_rate: decoded.sample_rate,
                    }
                };
                if audio.samples.is_empty() || audio.sample_rate == 0 {
                    return Err(format!("{} audio is empty", source.name()));
                }
                result.audio.insert(*source, audio);
            }
            let sr = result.audio[source].sample_rate;
            if *high_hz > f64::from(sr) * 0.5 {
                return Err(format!(
                    "frequency band exceeds {} audio's Nyquist frequency ({} Hz)",
                    source.name(),
                    sr / 2
                ));
            }
        }
    }
    if requests
        .iter()
        .any(|r| matches!(r, FeatureRequest::Onsets(_)))
    {
        let json: Option<String> = sqlx::query_scalar("SELECT onsets_json FROM track_drum_onsets JOIN auth_visible_tracks USING (track_id) WHERE track_id = ?")
            .bind(track_id).fetch_optional(access.connection()).await.map_err(|e| e.to_string())?;
        let json = json
            .ok_or("drum analysis is unavailable; analyze the track before using drum effects")?;
        let onsets: BTreeMap<String, Vec<f64>> =
            serde_json::from_str(&json).map_err(|e| format!("invalid drum analysis: {e}"))?;
        for request in requests {
            let FeatureRequest::Onsets(drum) = request else {
                continue;
            };
            let times = onsets
                .get(drum.name())
                .ok_or_else(|| format!("drum analysis has no {} event channel", drum.name()))?;
            if times.iter().any(|v| !v.is_finite()) || times.windows(2).any(|w| w[0] > w[1]) {
                return Err("drum analysis timestamps must be finite and ordered".into());
            }
            let beats = times
                .iter()
                .map(|t| result.clock.beat_at(*t))
                .collect::<p::Result<_>>()
                .map_err(|e| e.to_string())?;
            result.onsets.insert(*drum, beats);
        }
    }
    if requests.contains(&FeatureRequest::Harmony) {
        let json: Option<String> = sqlx::query_scalar("SELECT sections_json FROM track_roots JOIN auth_visible_tracks USING (track_id) WHERE track_id = ?")
            .bind(track_id).fetch_optional(access.connection()).await.map_err(|e| e.to_string())?;
        let json = json.ok_or(
            "harmony analysis is unavailable; analyze the track before using harmony effects",
        )?;
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&json).map_err(|e| format!("invalid harmony analysis: {e}"))?;
        let mut sections = Vec::new();
        for entry in entries {
            let start = entry["start"]
                .as_f64()
                .ok_or("harmony section needs a start")?;
            let end = entry["end"]
                .as_f64()
                .ok_or("harmony section needs an end")?;
            if !start.is_finite() || !end.is_finite() || end <= start {
                return Err("invalid harmony section span".into());
            }
            let pitch = match entry.get("root") {
                None | Some(serde_json::Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .filter(|v| *v < 12)
                        .ok_or("invalid harmony pitch class")? as u8,
                ),
            };
            sections.push((
                result.clock.beat_at(start).map_err(|e| e.to_string())?,
                result.clock.beat_at(end).map_err(|e| e.to_string())?,
                pitch,
            ));
        }
        result.harmony = Some(sections);
    }
    Ok(Arc::new(result))
}
