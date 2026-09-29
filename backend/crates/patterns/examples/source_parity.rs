use luma_patterns::*;
use std::io::{self, BufRead};
fn main() {
    let library = standard_library();
    for line in io::stdin().lock().lines() {
        let result = (|| -> Result<serde_json::Value> {
            let raw: serde_json::Value =
                serde_json::from_str(&line.map_err(|e| Error(e.to_string()))?)
                    .map_err(|e| Error(e.to_string()))?;
            let clip: Clip =
                serde_json::from_value(raw["clip"].clone()).map_err(|e| Error(e.to_string()))?;
            let cells: Vec<Cell> =
                serde_json::from_value(raw["cells"].clone()).map_err(|e| Error(e.to_string()))?;
            let beats: Vec<f64> =
                serde_json::from_value(raw["beats"].clone()).map_err(|e| Error(e.to_string()))?;
            let score = Score {
                clips: std::collections::BTreeMap::from([("comparison".into(), clip)]),
            };
            let program =
                score.prepare_clip(&library, "comparison", &Default::default(), &cells)?;
            let values = beats
                .iter()
                .map(|beat| program.evaluate(*beat))
                .collect::<Result<Vec<_>>>()?;
            serde_json::to_value(values).map_err(|e| Error(e.to_string()))
        })();
        println!(
            "{}",
            match result {
                Ok(v) => serde_json::json!({"ok":v}),
                Err(e) => serde_json::json!({"error":e.to_string()}),
            }
        );
    }
}
