//! Dry run of the curve format migration: every clip's `envelope`, `time`
//! and `hit` inputs move to the one curve format (`luma_patterns::Curve`).
//!
//! Opens a copy of the library database read-only, converts every clip with
//! `luma_lib::migration::curve_points`, and writes a report: `report.json`
//! (every curve that is not exact, and every clip that cannot convert) and
//! `proposed_changes.sql`, one transaction that `curve_points_apply` runs.
//! It writes no database and runs no SQL.
//!
//! ```text
//! cargo +1.97.1 run --release --bin curve_points_dry_run -- \
//!     --db /path/to/luma-copy.db --out /path/to/report
//! ```
use luma_lib::migration::curve_points::{convert, is_curve_type, EXACT};
use serde_json::{json, Value as Json};
use sqlx::{sqlite::SqliteConnectOptions, sqlite::SqlitePoolOptions, Row};
use std::{collections::BTreeMap, fmt::Write, fs, path::PathBuf};

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut database = None;
    let mut output = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => database = args.next().map(PathBuf::from),
            "--out" => output = args.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let database = database.ok_or("--db is required")?;
    let output = output.ok_or("--out is required")?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            // Immutable: SQLite neither locks the copy nor adds WAL files.
            SqliteConnectOptions::new()
                .filename(&database)
                .read_only(true)
                .immutable(true),
        )
        .await
        .map_err(|e| format!("cannot open {} read-only: {e}", database.display()))?;
    let rows = sqlx::query("SELECT id, uid, inputs_json FROM clips ORDER BY id")
        .fetch_all(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut inexact = Vec::new();
    let mut failed = Vec::new();
    let mut sql = String::new();
    writeln!(
        sql,
        "-- Curve format migration, generated {now} from {}.\n\
         -- SQLite. Not run by the dry run. Read report.json first.\n\
         PRAGMA foreign_keys = ON;\nBEGIN IMMEDIATE;",
        database.display()
    )
    .unwrap();
    let text = |value: &str| format!("'{}'", value.replace('\'', "''"));
    for row in &rows {
        *counts.entry("clips").or_default() += 1;
        let id: String = row.get("id");
        let uid: String = row.get("uid");
        let stored: String = row.get("inputs_json");
        let mut inputs: Json = match serde_json::from_str(&stored) {
            Ok(inputs) => inputs,
            Err(e) => {
                failed.push(json!({"clip": id, "error": format!("inputs_json: {e}")}));
                continue;
            }
        };
        let mut changed = false;
        let Some(map) = inputs.as_object_mut() else {
            continue;
        };
        for (key, input) in map.iter_mut() {
            let Some(kind) = input["type"].as_str().filter(|k| is_curve_type(k)) else {
                continue;
            };
            let kind = kind.to_string();
            *counts.entry("curves").or_default() += 1;
            *by_kind.entry(kind.clone()).or_default() += 1;
            match convert(&kind, &input["value"]) {
                Ok(converted) => {
                    if converted.smoothstep {
                        *counts.entry("curves with the old smooth ease").or_default() += 1;
                    }
                    if converted.max_diff <= EXACT {
                        *counts.entry("curves exact").or_default() += 1;
                    } else {
                        *counts.entry("curves not exact").or_default() += 1;
                        inexact.push(json!({
                            "clip": id,
                            "input": key,
                            "type": kind,
                            "max_diff": converted.max_diff,
                            "reasons": converted.reasons,
                            "old": input["value"],
                            "new": converted.value,
                        }));
                    }
                    if input["value"] != converted.value {
                        *counts.entry("curves rewritten").or_default() += 1;
                        input["value"] = converted.value;
                        changed = true;
                    }
                }
                Err(error) => {
                    *counts.entry("curves that cannot convert").or_default() += 1;
                    failed.push(json!({
                        "clip": id,
                        "input": key,
                        "type": kind,
                        "error": error,
                        "old": input["value"],
                    }));
                }
            }
        }
        if changed {
            *counts.entry("clips rewritten").or_default() += 1;
            writeln!(
                sql,
                "UPDATE clips SET inputs_json = {}, updated_at = {} WHERE id = {} AND uid = {};",
                text(&inputs.to_string()),
                text(&now),
                text(&id),
                text(&uid)
            )
            .unwrap();
        }
    }
    writeln!(sql, "COMMIT;").unwrap();

    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let report = json!({
        "database": database.display().to_string(),
        "generated": now,
        "counts": counts,
        "curves_by_type": by_kind,
        "not_exact": inexact,
        "cannot_convert": failed,
    });
    fs::write(
        output.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    fs::write(output.join("proposed_changes.sql"), sql).map_err(|e| e.to_string())?;
    for (name, n) in &counts {
        println!("{name}: {n}");
    }
    for (kind, n) in &by_kind {
        println!("{kind} curves: {n}");
    }
    let mut reasons: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    for curve in &inexact {
        let reason = curve["reasons"][0].as_str().unwrap_or("").to_string();
        let reason = reason
            .split(" at x ")
            .next()
            .unwrap_or_default()
            .to_string();
        let entry = reasons.entry(reason).or_default();
        entry.0 += 1;
        entry.1 = entry.1.max(curve["max_diff"].as_f64().unwrap_or(0.));
    }
    for (reason, (n, worst)) in reasons {
        println!("not exact, {reason}: {n} (largest difference {worst:.4})");
    }
    println!("report: {}", output.join("report.json").display());
    Ok(())
}
