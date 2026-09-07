//! `cargo run -p luma-patterns --release --example frame-budget < cells.json`
//! Input: a JSON array of host-resolved Cells, e.g. preview_composable_pattern.cells.
use luma_patterns::{standard_library, Cell, Frame, PreparedGraph};
use std::{collections::BTreeMap, hint::black_box, io, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cells: Vec<Cell> = serde_json::from_reader(io::stdin())?;
    let library = standard_library();
    for definition in ["chase", "dissolve_flash"] {
        let start = Instant::now();
        let program = PreparedGraph::new(
            &library,
            definition,
            &BTreeMap::new(),
            Frame {
                features: None,
                cells: &cells,
                beat: 0.0,
                clip_start: 0.0,
                clip_duration: 4.0,
                seed: 42,
            },
        )?;
        let preparation_ms = start.elapsed().as_secs_f64() * 1000.0;
        for n in 0..100 {
            black_box(program.evaluate(n as f64 / 30.0)?);
        }
        let mut micros = Vec::new();
        for n in 0..1200 {
            let start = Instant::now();
            black_box(program.evaluate(n as f64 / 30.0)?);
            micros.push(start.elapsed().as_secs_f64() * 1_000_000.0);
        }
        micros.sort_by(f64::total_cmp);
        println!(
            "{}",
            serde_json::json!({
                "definition": definition, "cells": cells.len(), "frames": micros.len(),
                "preparationMs": preparation_ms,
                "frameMedianUs": micros[micros.len() / 2],
                "frameP99Us": micros[micros.len() * 99 / 100],
                "dynamicSteps": program.dynamic_step_count(),
            })
        );
    }
    Ok(())
}
