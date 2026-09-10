//! Track data for canonical graphs. Load only requested sources under the
//! score's authorized snapshot; all frame sampling is pure and seekable.
use crate::{database::local::venue_access::AuthorizedVenue, storage::StorageRoot};
use luma_patterns::{self as p, FeatureRequest, FeatureSample};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug)]
pub(crate) struct TrackFeatures {
    clock: p::BeatTimeline,
    timing: Arc<p::TrackTiming>,
    audio: AudioSources,
    onsets: BTreeMap<p::Drum, p::EventTimes>,
    harmony: Option<Vec<(f64, f64, Option<u8>)>>,
}
impl TrackFeatures {
    /// Adapt already resident host data to the same source used by score playback.
    pub(crate) fn from_resident(
        ctx: &super::ResidentContext,
        clock: p::BeatTimeline,
        requests: &[FeatureRequest],
    ) -> Result<Arc<Self>, String> {
        let raw_times = |values: &[f32]| {
            p::EventTimes::new(values.iter().copied().map(f64::from).collect::<Vec<_>>())
        };
        let beat_times = |values: &[f32]| -> p::Result<p::EventTimes> {
            p::EventTimes::new(
                values
                    .iter()
                    .map(|v| clock.beat_at(f64::from(*v)))
                    .collect::<p::Result<Vec<_>>>()?,
            )
        };
        let (beats, downbeats, bpm) = ctx
            .beat_grid
            .as_ref()
            .map(|grid| (grid.beats.as_slice(), grid.downbeats.as_slice(), grid.bpm))
            .unwrap_or((&[], &[], 120.));
        let timing = p::TrackTiming::new(
            clock.clone(),
            raw_times(beats).map_err(|e| e.to_string())?,
            raw_times(downbeats).map_err(|e| e.to_string())?,
            f64::from(bpm),
        )
        .map_err(|e| e.to_string())?;
        let mut result = Self {
            clock: clock.clone(),
            timing: Arc::new(timing),
            audio: AudioSources::resident(ctx),
            onsets: BTreeMap::new(),
            harmony: None,
        };
        for request in requests {
            match request {
                FeatureRequest::Spectrum { source, .. } | FeatureRequest::Band { source, .. } => {
                    result.audio.prepare(source)?;
                }
                FeatureRequest::Onsets(drum) => {
                    let values = ctx
                        .drum_onsets
                        .get(drum.name())
                        .ok_or_else(|| format!("{} onsets were not prepared", drum.name()))?;
                    result
                        .onsets
                        .insert(*drum, beat_times(values).map_err(|e| e.to_string())?);
                }
                FeatureRequest::Harmony => {
                    result.harmony = Some(
                        ctx.chord_sections
                            .iter()
                            .map(|(a, b, pitch)| {
                                Ok((
                                    clock.beat_at(f64::from(*a))?,
                                    clock.beat_at(f64::from(*b))?,
                                    *pitch,
                                ))
                            })
                            .collect::<p::Result<Vec<_>>>()
                            .map_err(|e| e.to_string())?,
                    );
                }
                FeatureRequest::Timing => {}
            }
        }
        Ok(Arc::new(result))
    }
    pub(crate) fn inspection_sources(&self) -> AudioSources {
        self.audio.clone()
    }
}

/// Playback and inspection share immutable PCM and each prepared filter chain.
#[derive(Clone, Debug, Default)]
pub(crate) struct AudioSources {
    raw: BTreeMap<p::AudioSource, super::ResidentAudio>,
    processed: Vec<(p::AudioInput, super::ResidentAudio)>,
}
impl AudioSources {
    pub(crate) fn resident(ctx: &super::ResidentContext) -> Self {
        let mut raw = BTreeMap::new();
        if let Some(audio) = &ctx.audio {
            raw.insert(p::AudioSource::Mix, audio.clone());
        }
        for source in [
            p::AudioSource::Bass,
            p::AudioSource::Drums,
            p::AudioSource::Vocals,
            p::AudioSource::Other,
        ] {
            if let Some(audio) = ctx.stems.get(source.name()) {
                raw.insert(source, audio.clone());
            }
        }
        Self {
            raw,
            processed: vec![],
        }
    }
    pub(crate) fn prepare(&mut self, source: &p::AudioInput) -> Result<(), String> {
        source.validate().map_err(|e| e.to_string())?;
        let audio = self
            .raw
            .get(&source.source())
            .ok_or_else(|| format!("{} audio was not prepared", source.name()))?;
        if source.filters().is_empty() || self.processed.iter().any(|(key, _)| key == source) {
            return Ok(());
        }
        let samples =
            crate::audio::filters::apply_chain(&audio.samples, audio.sample_rate, source)?;
        self.processed.push((
            source.clone(),
            super::ResidentAudio {
                samples: Arc::new(samples),
                sample_rate: audio.sample_rate,
            },
        ));
        Ok(())
    }
    pub(crate) fn get(&self, source: &p::AudioInput) -> p::Result<&super::ResidentAudio> {
        let audio = if source.filters().is_empty() {
            self.raw.get(&source.source())
        } else {
            self.processed
                .iter()
                .find(|(key, _)| key == source)
                .map(|(_, audio)| audio)
        };
        audio.ok_or_else(|| p::Error(format!("{} audio was not prepared", source.name())))
    }

    pub(crate) async fn load(
        &mut self,
        access: &mut impl AuthorizedVenue,
        storage: &StorageRoot,
        track_id: &str,
        source: &p::AudioInput,
    ) -> Result<(), String> {
        source.validate().map_err(|e| e.to_string())?;
        let base = source.source();
        if !self.raw.contains_key(&base) {
            let path = crate::database::local::tracks::get_track_path_and_hash_for_connection(
                access.connection(),
                track_id,
            )
            .await?;
            let audio = if base == p::AudioSource::Mix {
                super::context::load_track_audio_cached(storage, &path.file_path, &path.track_hash)?
            } else {
                let file = storage.stem_pcm_path(&path.track_hash, source.name());
                let decoded = match crate::audio::read_pcm_file(&file) {
                    Ok(pcm) => Arc::new(crate::audio::decoder::DecodedAudio::from(pcm)),
                    Err(cache_error) => {
                        let source_file = storage
                            .stem_source_path(&path.track_hash, source.name())
                            .ok_or_else(|| {
                                format!(
                                    "{} stem unavailable; analyze the track's stems: {cache_error}",
                                    source.name()
                                )
                            })?;
                        // Decode the exact requested stem; never substitute the mix.
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
            self.raw.insert(base, audio);
        }
        self.prepare(source)
    }
}
impl p::FeatureSource for TrackFeatures {
    fn onsets(&self, drum: p::Drum) -> p::Result<p::EventTimes> {
        self.onsets
            .get(&drum)
            .cloned()
            .ok_or_else(|| p::Error(format!("{} onsets were not prepared", drum.name())))
    }

    fn sample(&self, request: &FeatureRequest, beat: f64) -> p::Result<FeatureSample> {
        match request {
            FeatureRequest::Timing => Ok(FeatureSample::Timing(self.timing.clone())),
            FeatureRequest::Spectrum { source, hold_edges } => {
                let audio = self.audio.get(source)?;
                let (bins, bin_hz) = crate::audio::spectrum::sample(
                    &audio.samples,
                    audio.sample_rate,
                    self.clock.seconds_at(beat)?,
                    *hold_edges,
                )
                .map_err(p::Error)?;
                Ok(FeatureSample::Spectrum { bins, bin_hz })
            }
            FeatureRequest::Band {
                source,
                low_hz,
                high_hz,
            } => {
                let audio = self.audio.get(source)?;
                let seconds = self.clock.seconds_at(beat)?;
                Ok(FeatureSample::Energy(f64::from(
                    crate::audio::spectrum::sample_band(
                        &audio.samples,
                        audio.sample_rate,
                        seconds,
                        [*low_hz as f32, *high_hz as f32],
                    )
                    .map_err(p::Error)?,
                )))
            }
            FeatureRequest::Onsets(drum) => {
                let events = self
                    .onsets
                    .get(drum)
                    .ok_or_else(|| p::Error(format!("{} onsets were not prepared", drum.name())))?;
                let events = events.as_slice();
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
    grid: &crate::models::node_graph::BeatGrid,
    requests: &[FeatureRequest],
) -> Result<Arc<TrackFeatures>, String> {
    let mut result = TrackFeatures {
        clock: grid.timeline().map_err(|e| e.to_string())?,
        timing: Arc::new(grid.timing().map_err(|e| e.to_string())?),
        audio: AudioSources::default(),
        onsets: BTreeMap::new(),
        harmony: None,
    };
    for request in requests {
        let (source, high_hz) = match request {
            FeatureRequest::Band {
                source, high_hz, ..
            } => (source, Some(*high_hz)),
            FeatureRequest::Spectrum { source, .. } => (source, None),
            _ => continue,
        };
        result.audio.load(access, storage, track_id, source).await?;
        let sr = result
            .audio
            .get(source)
            .map_err(|e| e.to_string())?
            .sample_rate;
        if high_hz.is_some_and(|high| high > f64::from(sr) * 0.5) {
            return Err(format!(
                "frequency band exceeds {} audio's Nyquist frequency ({} Hz)",
                source.name(),
                sr / 2
            ));
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
            let beats: Vec<_> = times
                .iter()
                .map(|t| result.clock.beat_at(*t))
                .collect::<p::Result<_>>()
                .map_err(|e| e.to_string())?;
            result
                .onsets
                .insert(*drum, p::EventTimes::new(beats).map_err(|e| e.to_string())?);
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

#[cfg(test)]
mod tests {
    use super::*;
    use p::FeatureSource;

    #[test]
    fn audio_chains_share_exact_stem_samples_with_spectra_and_spectrograms() {
        let clock = p::BeatTimeline::new(vec![0., 0.5, 1., 1.5], 0.).unwrap();
        let samples = (0..8000)
            .map(|i| {
                let phase = std::f32::consts::TAU * i as f32 / 8000.;
                0.4 * (phase * 100.).sin() + 0.2 * (phase * 2000.).sin()
            })
            .collect::<Vec<_>>();
        let original = samples.clone();
        let mut features = TrackFeatures {
            clock: clock.clone(),
            timing: Arc::new(
                p::TrackTiming::new(
                    clock,
                    p::EventTimes::new(vec![0., 0.5, 1.]).unwrap(),
                    p::EventTimes::new(vec![0.]).unwrap(),
                    120.,
                )
                .unwrap(),
            ),
            audio: AudioSources {
                raw: BTreeMap::from([
                    (
                        p::AudioSource::Mix,
                        super::super::ResidentAudio {
                            samples: Arc::new(vec![0.; samples.len()]),
                            sample_rate: 8000,
                        },
                    ),
                    (
                        p::AudioSource::Drums,
                        super::super::ResidentAudio {
                            samples: Arc::new(samples),
                            sample_rate: 8000,
                        },
                    ),
                ]),
                processed: vec![],
            },
            onsets: BTreeMap::new(),
            harmony: None,
        };
        let source = p::AudioInput::from(p::AudioSource::Drums)
            .filtered(p::AudioFilter::Highpass { cutoff_hz: 50. })
            .unwrap()
            .filtered(p::AudioFilter::Lowpass { cutoff_hz: 400. })
            .unwrap();
        features.audio.prepare(&source).unwrap();
        let prepared = features.audio.get(&source).unwrap().samples.clone();
        let expected = crate::audio::lowpass_filter(
            &crate::audio::highpass_filter(&original, 50., 8000.),
            400.,
            8000.,
        );
        assert_eq!(*prepared, expected);
        assert_eq!(
            *features.audio.raw[&p::AudioSource::Drums].samples,
            original
        );
        features.audio.prepare(&source).unwrap();
        assert_eq!(features.audio.processed.len(), 1);
        assert!(Arc::ptr_eq(
            &prepared,
            &features.audio.get(&source).unwrap().samples
        ));
        let mut inspection = features.inspection_sources();
        inspection.prepare(&source).unwrap();
        let inspected = inspection.get(&source).unwrap();
        assert!(
            Arc::ptr_eq(&prepared, &inspected.samples),
            "inspection rebuilt the filter chain"
        );
        let mel =
            crate::audio::melspec::inspect(&inspected.samples, inspected.sample_rate, (0.2, 0.8))
                .unwrap();
        assert_eq!(mel.span, (0.2, 0.8));
        assert_eq!(
            mel.data,
            crate::audio::generate_melspec(
                &crate::audio::FftService::new(),
                &expected[1600..6400],
                8000,
                mel.width,
                mel.height
            )
        );
        let raw = crate::audio::melspec::inspect(&original, 8000, (0.2, 0.8)).unwrap();
        assert!(
            mel.data
                .iter()
                .zip(&raw.data)
                .any(|(a, b)| (a - b).abs() > 0.1),
            "the spectrogram ignored its connected filters"
        );
        let request = FeatureRequest::Spectrum {
            source: source.clone(),
            hold_edges: false,
        };
        let mut results = Vec::new();
        for beat in [0.8, 0.25, 0.8, 3.] {
            let FeatureSample::Spectrum { bins, bin_hz } = features.sample(&request, beat).unwrap()
            else {
                panic!()
            };
            let reference =
                crate::audio::spectrum::sample(&expected, 8000, beat * 0.5, false).unwrap();
            assert_eq!((bins.clone(), bin_hz), reference);
            results.push(bins);
        }
        assert_eq!(results[0], results[2]);
        assert!(results[0].iter().any(|v| *v > 0.));
        assert!(results[3].iter().all(|v| *v == 0.));
        let missing = p::AudioInput::from(p::AudioSource::Bass)
            .filtered(p::AudioFilter::Lowpass { cutoff_hz: 200. })
            .unwrap();
        assert!(features
            .audio
            .prepare(&missing)
            .unwrap_err()
            .contains("bass audio was not prepared"));
    }

    #[test]
    fn spectra_do_not_substitute_the_mix_for_a_missing_stem() {
        let source = TrackFeatures {
            clock: p::BeatTimeline::new(vec![0., 0.5], 0.).unwrap(),
            timing: Arc::new(
                p::TrackTiming::new(
                    p::BeatTimeline::new(vec![0., 0.5], 0.).unwrap(),
                    p::EventTimes::new(vec![0., 0.5]).unwrap(),
                    p::EventTimes::new(vec![0.]).unwrap(),
                    120.,
                )
                .unwrap(),
            ),
            audio: AudioSources {
                raw: BTreeMap::from([(
                    p::AudioSource::Mix,
                    super::super::ResidentAudio {
                        samples: Arc::new(vec![1.; 4096]),
                        sample_rate: 8000,
                    },
                )]),
                processed: Vec::new(),
            },
            onsets: BTreeMap::new(),
            harmony: None,
        };
        let error = source
            .sample(
                &FeatureRequest::Spectrum {
                    source: p::AudioSource::Bass.into(),
                    hold_edges: false,
                },
                0.5,
            )
            .unwrap_err();
        assert!(error.0.contains("bass audio was not prepared"));
        let request = FeatureRequest::Spectrum {
            source: p::AudioSource::Mix.into(),
            hold_edges: false,
        };
        let FeatureSample::Spectrum { bins, bin_hz } = source.sample(&request, 2.).unwrap() else {
            panic!()
        };
        assert!(bins.iter().all(|v| *v == 0.));
        assert_eq!(bin_hz, 8000. / 2048.);
        let held = FeatureRequest::Spectrum {
            source: p::AudioSource::Mix.into(),
            hold_edges: true,
        };
        let FeatureSample::Spectrum { bins, .. } = source.sample(&held, 2.).unwrap() else {
            panic!()
        };
        assert!(bins[0] > 0.);
    }
}
