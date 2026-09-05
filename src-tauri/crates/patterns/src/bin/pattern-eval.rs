//! JSON-in/JSON-out access to the same graph evaluator used by contract tests.
//! Useful for inspecting a definition or rendering a saved score during the
//! migration; never opens a user's library or sends device output.
use luma_patterns::{standard_library, Cell, Frame, Library, MappingSpec, Score, Value};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::{self, Read},
};

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Catalog,
    Evaluate {
        #[serde(default)]
        library: Option<Library>,
        definition: String,
        #[serde(default)]
        cells: Vec<Cell>,
        #[serde(default)]
        inputs: BTreeMap<String, Value>,
        beats: Vec<f64>,
        #[serde(default)]
        clip_start: f64,
        #[serde(default)]
        seed: u64,
    },
    PreviewScore {
        score: Score,
        clip: String,
        beats: Vec<f64>,
        mapping: MappingSpec,
        cells: Vec<Cell>,
    },
}
fn run() -> Result<serde_json::Value, String> {
    let mut json = String::new();
    io::stdin()
        .take(16 * 1024 * 1024)
        .read_to_string(&mut json)
        .map_err(|e| e.to_string())?;
    let request: Request = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let result = match request {
        Request::Catalog => serde_json::to_value(standard_library()),
        Request::Evaluate {
            library,
            definition,
            cells,
            inputs,
            beats,
            clip_start,
            seed,
        } => {
            if beats.len() > 10000 {
                return Err("preview is limited to 10,000 frames".into());
            }
            let library = library.unwrap_or_else(standard_library);
            let frames = beats
                .into_iter()
                .map(|beat| {
                    library.evaluate(
                        &definition,
                        &inputs,
                        Frame {
                            cells: &cells,
                            beat,
                            clip_start,
                            seed,
                        },
                    )
                })
                .collect::<luma_patterns::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            serde_json::to_value(frames)
        }
        Request::PreviewScore {
            score,
            clip,
            beats,
            mapping,
            cells,
        } => {
            if beats.len() > 10000 {
                return Err("preview is limited to 10,000 frames".into());
            }
            let inputs = BTreeMap::from([("mapping".into(), Value::Mapping(mapping))]);
            let library = standard_library();
            score
                .validate(&library)
                .map_err(|error| error.to_string())?;
            let frames = beats
                .into_iter()
                .map(|beat| score.evaluate_clip(&library, &clip, &inputs, beat, &cells))
                .collect::<luma_patterns::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            serde_json::to_value(frames)
        }
    };
    result.map_err(|e| e.to_string())
}
fn main() {
    match run() {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
