//! Capture actual per-head output for one representative clip of every effect
//! used in a venue. The complete library must be snapshotted independently.
use luma_lib::{
    eval::{self, Arena},
    headless_host::{boot, HostConfig},
};
use serde_json::json;
use sqlx::Row;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let venue = args.next().ok_or("venue id required")?;
    let output = PathBuf::from(args.next().ok_or("output directory required")?);
    if output.exists() {
        return Err("output directory already exists".into());
    }
    let services = boot(&HostConfig::parse_args(args)?).await?;
    let pool = &services.db().0;
    let rows = sqlx::query(
        "SELECT t.*, s.track_id, p.name AS pattern_name FROM track_scores t
         JOIN scores s ON s.id=t.score_id JOIN patterns p ON p.id=t.pattern_id
         WHERE s.venue_id=? ORDER BY t.pattern_id, (t.end_time-t.start_time) DESC, t.id",
    )
    .bind(&venue)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let mut seen = std::collections::BTreeSet::new();
    let mut manifest = Vec::new();
    for row in rows {
        let pattern: String = row.get("pattern_id");
        if !seen.insert(pattern.clone()) {
            continue;
        }
        let clip: String = row.get("id");
        let track: String = row.get("track_id");
        let name: String = row.get("pattern_name");
        let start = row.get::<f64, _>("start_time") as f32;
        let end = row.get::<f64, _>("end_time") as f32;
        let graph = luma_lib::services::graph_documents::load_visible_graph_document(
            pool,
            &pattern,
            Some(&venue),
            None,
        )
        .await
        .map_err(|e| e.to_string())?
        .graph;
        let overrides: serde_json::Value =
            serde_json::from_str(row.get::<&str, _>("args_json")).map_err(|e| e.to_string())?;
        let mut values: std::collections::HashMap<String, serde_json::Value> = graph
            .args
            .iter()
            .map(|a| (a.id.clone(), a.default_value.clone()))
            .collect();
        if let Some(args) = overrides.as_object() {
            values.extend(args.clone());
        }
        let grid = luma_lib::services::tracks::get_track_beats(pool, &track).await?;
        let (ctx, ids) = eval::context::build_resident_context(
            pool,
            pool,
            services.storage(),
            services.fixtures_root(),
            &track,
            &venue,
            Some(&clip),
            &graph.nodes,
            &graph.edges,
            &values,
            (start, end),
            grid,
        )
        .await;
        let plan = eval::compile::compile_pattern(&graph.nodes, &graph.edges, &values, ctx, ids)
            .map_err(|e| format!("{name}: {e:?}"))?;
        let times: Vec<f32> = (0..65)
            .map(|i| start + (end - start) * (i as f32 / 65.0))
            .collect();
        let frames = eval::eval(&plan, &times, &mut Arena::default());
        let lit = frames
            .iter()
            .any(|f| f.primitives.values().any(|p| p.dimmer > 0.0001));
        let data = json!({"pattern_id":pattern,"name":name,"clip_id":clip,"track_id":track,"venue_id":venue,
            "start":start,"end":end,"args":values,"graph":graph,"times":times,"frames":frames,
            "writes":{"color":plan.outputs.color.is_some(),"dimmer":plan.outputs.dimmer.is_some(),
                "position":plan.outputs.position.is_some(),"strobe":plan.outputs.strobe.is_some(),"speed":plan.outputs.speed.is_some()}});
        let file = format!("{pattern}.json");
        std::fs::write(
            output.join(&file),
            serde_json::to_vec(&data).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        manifest.push(json!({"pattern":pattern,"name":name,"clip":clip,"file":file,"heads":plan.n,"has_light":lit}));
        eprintln!("Captured {name}: {} heads, lit={lit}", plan.n);
    }
    std::fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
