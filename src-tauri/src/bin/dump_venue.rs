//! Headless venue dumper.
//!
//! Emits JSON: venue id/name, every fixture (with 3D position + rotation),
//! and every fixture group + its member fixture IDs.
//!
//! Drives the 2D box-projection in the trace-viewer canvas. luma-lighting-
//! model only needs positions + grouping, not the full GDTF channel layout.
//!
//! Standalone — does NOT depend on `luma_lib::database` (private) or any
//! Tauri state. Just opens the SQLite read-only and runs three queries.
//!
//! Usage:
//!     cargo run --release --bin dump_venue -- --venue-id <UUID> [--output PATH]
//!
//! Defaults to the macOS app DB at
//!     ~/Library/Application Support/com.luma.luma/luma.db
//! Override with --db PATH for tests or non-macOS hosts.

use std::path::PathBuf;

use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::FromRow;

#[derive(Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct FixtureRow {
    id: String,
    venue_id: String,
    manufacturer: String,
    model: String,
    mode_name: String,
    label: Option<String>,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
    num_channels: i64,
}

#[derive(Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct GroupRow {
    id: String,
    name: Option<String>,
    axis_lr: Option<f64>,
    axis_fb: Option<f64>,
    axis_ab: Option<f64>,
}

#[derive(Serialize)]
struct GroupDump {
    #[serde(flatten)]
    group: GroupRow,
    #[serde(rename = "memberIds")]
    member_ids: Vec<String>,
}

#[derive(Serialize)]
struct VenueDump {
    venue_id: String,
    venue_name: Option<String>,
    fixtures: Vec<FixtureRow>,
    groups: Vec<GroupDump>,
}

struct Args {
    venue_id: String,
    db_path: PathBuf,
    output: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut venue_id: Option<String> = None;
    let mut db_path: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--venue-id" => venue_id = it.next(),
            "--db" => db_path = it.next().map(PathBuf::from),
            "--output" | "-o" => output = it.next().map(PathBuf::from),
            "--help" | "-h" => {
                eprintln!("usage: dump_venue --venue-id <UUID> [--db PATH] [--output PATH]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }

    Ok(Args {
        venue_id: venue_id.ok_or("--venue-id is required")?,
        db_path: db_path.unwrap_or_else(default_luma_db),
        output,
    })
}

fn default_luma_db() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/com.luma.luma/luma.db")
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let args = parse_args()?;

    if !args.db_path.exists() {
        return Err(format!("luma DB not found at {}", args.db_path.display()));
    }

    let connect = SqliteConnectOptions::new()
        .filename(&args.db_path)
        .read_only(true)
        .immutable(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(connect)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;

    let venue_name: Option<String> = sqlx::query_scalar("SELECT name FROM venues WHERE id = ?")
        .bind(&args.venue_id)
        .fetch_optional(&pool)
        .await
        .map_err(|e| format!("venue lookup failed: {e}"))?;

    let fixtures: Vec<FixtureRow> = sqlx::query_as(
        "SELECT id, venue_id, manufacturer, model, mode_name, label,
                pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, num_channels
         FROM fixtures WHERE venue_id = ?",
    )
    .bind(&args.venue_id)
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("fixtures query failed: {e}"))?;

    let group_rows: Vec<GroupRow> = sqlx::query_as(
        "SELECT id, name, axis_lr, axis_fb, axis_ab
         FROM fixture_groups WHERE venue_id = ? ORDER BY display_order",
    )
    .bind(&args.venue_id)
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("groups query failed: {e}"))?;

    let mut groups: Vec<GroupDump> = Vec::with_capacity(group_rows.len());
    for group in group_rows {
        // DISTINCT: per-head membership stores one row per head.
        let members: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT fixture_id FROM fixture_group_members WHERE group_id = ?",
        )
        .bind(&group.id)
        .fetch_all(&pool)
        .await
        .map_err(|e| format!("group members query failed: {e}"))?;
        groups.push(GroupDump {
            group,
            member_ids: members,
        });
    }

    let dump = VenueDump {
        venue_id: args.venue_id.clone(),
        venue_name,
        fixtures,
        groups,
    };
    let json = serde_json::to_string_pretty(&dump).map_err(|e| format!("serialize failed: {e}"))?;

    match args.output {
        Some(path) => {
            std::fs::write(&path, json).map_err(|e| format!("write failed: {e}"))?;
            eprintln!(
                "wrote {} fixtures, {} groups → {}",
                dump.fixtures.len(),
                dump.groups.len(),
                path.display()
            );
        }
        None => println!("{json}"),
    }

    Ok(())
}
