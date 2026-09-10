use luma_patterns::*;
use std::collections::BTreeMap;
#[derive(Debug)]
struct Analysis;
impl FeatureSource for Analysis {
    fn sample(&self, request: &FeatureRequest, beat: f64) -> Result<FeatureSample> {
        Ok(match request {
            FeatureRequest::Band { .. } => FeatureSample::Energy(0.5 + 0.4 * beat.sin()),
            FeatureRequest::Harmony => {
                FeatureSample::PitchClass(Some(beat.floor().rem_euclid(12.0) as u8))
            }
            FeatureRequest::Onsets(_) => FeatureSample::Onset(if beat >= 0.0 {
                Some((beat.floor(), beat.floor() as u64))
            } else {
                None
            }),
        })
    }
}
fn main() {
    let library = standard_library();
    let cells: Vec<_> = (0..9)
        .rev()
        .map(|i| Cell {
            id: format!("head-{i}"),
            group: "bars".into(),
            world: [i as f64, (i % 3) as f64, i as f64],
            uvz: [i as f64, (i % 3) as f64, i as f64],
        })
        .collect();
    let times = [2.0, 2.03125, 2.5, 3.0, 3.999, 4.0, 5.5, 6.25, 9.5];
    let mut score = Score::default();
    for (id, definition) in &library.definitions {
        if definition.playable() && definition.inputs.values().all(|i| i.default.is_some()) {
            for variant in 0..2 {
                let key = format!("sample-{id}-{variant}");
                score.insert_effect(&library, id, &key, 2.0, 8.0).unwrap();
                let clip = score.clips.get_mut(&key).unwrap();
                clip.seed = 427;
                if variant == 1 {
                    for (key, input) in &definition.inputs {
                        let value = match input.default.as_ref().unwrap() {
                            Value::Color(_) => Some(Value::Color([0.3, 0.1, 0.5])),
                            _ if key == "grid_aligned" => Some(Value::Boolean(true)),
                            _ if key == "delay" => Some(Value::Beats(0.25)),
                            _ if key == "pan" => Some(Value::Number(45.0)),
                            _ if key == "tilt" => Some(Value::Number(-20.0)),
                            _ => None,
                        };
                        if let Some(value) = value {
                            clip.inputs.insert(key.clone(), value);
                        }
                    }
                }
            }
        }
    }
    let lib = score.library(&library).unwrap();
    let mut samples = BTreeMap::new();
    for (id, clip) in &score.clips {
        let values: Vec<_> = times
            .iter()
            .map(|beat| {
                lib.evaluate(
                    &clip.graph,
                    &clip.inputs,
                    Frame {
                        features: Some(&Analysis),
                        cells: &cells,
                        beat: *beat,
                        clip_start: clip.start,
                        clip_duration: clip.duration,
                        seed: clip.seed,
                    },
                )
                .unwrap()["lighting"]
                    .clone()
            })
            .collect();
        samples.insert(id.clone(), values);
    }
    println!(
        "{}",
        serde_json::json!({"score":score,"cells":cells,"times":times,"samples":samples})
    );
}
