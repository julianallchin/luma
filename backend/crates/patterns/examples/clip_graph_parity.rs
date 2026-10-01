//! Clip graph side of the migration parity check
//! (`backend/scripts/check_clip_graphs_migration.py`, spec 8.5).
//!
//! Reads NDJSON on stdin and writes one JSON line per input line.
//!
//! Default mode: each line is `{"clip": <clip>, "cells": [...], "beats": [...]}`
//! with a clip in the clip graph shape. The answer is `{"ok": [frame, ...]}`,
//! one frame per beat as `PreparedClip::evaluate` gives it, or `{"error": text}`.
//!
//! `--check`: each line is a clip. The answer is `{"ok": null}` when the
//! checker passes, else `{"error": text}` with the checker's text.
//!
//! `--coordinate`: each line is `{"graph": <graph>, "node": id, "cells":
//! [...]}`. The answer is `{"ok": {cell id: value}}`: coordinate node `id`
//! per head at the clip's start (a migration reads a space's raw place).
use luma_patterns::*;
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

fn parse<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|e| Error(e.to_string()))
}

fn evaluate(library: &Library, raw: &serde_json::Value) -> Result<serde_json::Value> {
    let clip: Clip = parse(&raw["clip"])?;
    let cells: Vec<Cell> = parse(&raw["cells"])?;
    let beats: Vec<f64> = parse(&raw["beats"])?;
    let score = Score {
        clips: BTreeMap::from([("comparison".into(), clip)]),
    };
    let program = score.prepare_clip(library, "comparison", &cells)?;
    let frames = beats
        .iter()
        .map(|beat| program.evaluate(*beat))
        .collect::<Result<Vec<_>>>()?;
    serde_json::to_value(frames).map_err(|e| Error(e.to_string()))
}

fn check(raw: &serde_json::Value) -> Result<serde_json::Value> {
    let clip: Clip = parse(raw)?;
    clip_graph::check_clip(&clip)?;
    Ok(serde_json::Value::Null)
}

fn coordinate(raw: &serde_json::Value) -> Result<serde_json::Value> {
    let graph: ClipGraph = parse(&raw["graph"])?;
    let cells: Vec<Cell> = parse(&raw["cells"])?;
    let node: String = parse(&raw["node"])?;
    let values = graph.coordinate_at_heads(
        &node,
        Frame {
            cells: &cells,
            features: None,
            beat: 0.,
            clip_start: 0.,
            clip_duration: 16.,
            seed: 0,
        },
    )?;
    Ok(cells
        .iter()
        .zip(values)
        .map(|(cell, value)| (cell.id.clone(), serde_json::json!(value)))
        .collect::<serde_json::Map<_, _>>()
        .into())
}

fn main() {
    let check_only = std::env::args().any(|arg| arg == "--check");
    let coordinates = std::env::args().any(|arg| arg == "--coordinate");
    let library = standard_library();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for line in io::stdin().lock().lines() {
        let result = line
            .map_err(|e| Error(e.to_string()))
            .and_then(|line| serde_json::from_str(&line).map_err(|e| Error(e.to_string())))
            .and_then(|raw: serde_json::Value| {
                if check_only {
                    check(&raw)
                } else if coordinates {
                    coordinate(&raw)
                } else {
                    evaluate(&library, &raw)
                }
            });
        let answer = match result {
            Ok(value) => serde_json::json!({ "ok": value }),
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
        writeln!(out, "{answer}").expect("stdout");
    }
}
