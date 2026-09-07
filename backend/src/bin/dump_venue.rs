//! Headless venue dumper.
//!
//! Emits JSON: venue id/name, every fixture (with 3D position + rotation),
//! and the venue's whole group tree — derived sets, the edits on them, and the
//! authored `fixture_groups` rows — each with its member fixture IDs.
//!
//! Drives the 2D box-projection in the trace-viewer canvas. luma-lighting-
//! model only needs positions + grouping, not the full QLC+ channel layout.
//!
//! Read-only, and read through the same merged group read the app uses
//! (`services::groups::GroupSources`) rather than a `fixture_groups` query of
//! its own: a dumper that saw only the authored table reported "no groups" for
//! every venue built by an agent.
//!
//! Usage:
//!     cargo run --release --bin dump_venue -- --venue-id <UUID> [--output PATH]
//!
//! Defaults to the macOS app DB at
//!     ~/Library/Application Support/com.luma.luma/luma.db
//! Override with --db PATH for tests or non-macOS hosts.

use std::path::PathBuf;

use glam::{DMat4, DVec3};
use luma_lib::database::local::venue_access::{Read, VenueAccess, VenueResource};
use luma_lib::models::groups::GroupOrigin;
use luma_lib::services::groups::GroupSources;
use luma_lib::storage::StorageRoot;
use luma_scene::coords::data_pose_of_d;
use luma_scene::venue::NodeSockets;
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

/// One node of the merged group tree, flat with `parentId`, parents first.
///
/// The axis fields are columns of an authored `fixture_groups` row, so a
/// derived node ships without them rather than with nulls a reader could take
/// for "centred".
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupDump {
    id: String,
    /// The snake_case name a selection expression uses; empty for a group
    /// nobody has named.
    name: String,
    label: String,
    /// The labels from the root down to this node, `/`-joined.
    path: String,
    parent_id: Option<String>,
    /// `derived`, `edited` or `manual`.
    origin: GroupOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    axis_lr: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    axis_fb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    axis_ab: Option<f64>,
    member_ids: Vec<String>,
}

/// One patched fixture as the grouping rule sees it: identity from the patch
/// list, role from the definition, placement from the solve — in data space
/// (+X stage right, +Y upstage, +Z up), metres.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureFactDump {
    id: String,
    label: Option<String>,
    manufacturer: String,
    model: String,
    role: String,
    /// The venue-graph node the fixture hangs on; `None` for the patch tray.
    parent: Option<String>,
    position: Option<[f64; 3]>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SocketDump {
    name: String,
    position: [f64; 3],
}

/// One structure node of the solved venue, data space.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PieceDump {
    node: String,
    label: Option<String>,
    kind: String,
    catalog_ref: Option<String>,
    parent: Option<String>,
    position: [f64; 3],
    sockets: Vec<SocketDump>,
}

#[derive(Serialize)]
struct VenueDump {
    venue_id: String,
    venue_name: Option<String>,
    fixtures: Vec<FixtureRow>,
    groups: Vec<GroupDump>,
    /// Solved facts: the same values `group_derivation` reads.
    fixture_facts: Vec<FixtureFactDump>,
    pieces: Vec<PieceDump>,
    stage_centre_x: f64,
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
    StorageRoot::from_env_default()
        .map(|r| r.luma_db_path())
        .unwrap_or_default()
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

    let fixtures_root = luma_lib::services::fixtures::resolve_fixtures_root_from(None)?;
    let mut access = VenueAccess::<Read>::read(&pool, VenueResource::Venue(&args.venue_id))
        .await
        .map_err(|e| format!("venue not readable: {e}"))?;
    let sources = GroupSources::read(&fixtures_root, &mut access)
        .await
        .map_err(|e| format!("group tree read failed: {e}"))?;
    let nodes = sources.hierarchy();
    let paths = luma_lib::services::groups::label_paths(&nodes);
    let groups: Vec<GroupDump> = nodes
        .iter()
        .map(|node| GroupDump {
            id: node.id.clone(),
            name: node.name.clone(),
            label: node.label.clone(),
            path: paths.get(&node.id).cloned().unwrap_or_default(),
            parent_id: node.parent_id.clone(),
            origin: node.origin,
            role: node.role.map(|role| role.as_str().to_string()),
            axis_lr: node.axis_lr,
            axis_fb: node.axis_fb,
            axis_ab: node.axis_ab,
            member_ids: node.fixtures.iter().map(|f| f.id.clone()).collect(),
        })
        .collect();

    // The same solve the group tree came from, read a second time for the raw
    // facts: the `fixtures` rows carry table positions (all zero), which say
    // nothing about where anything hangs.
    let solved = luma_lib::services::groups::solve(&fixtures_root, &mut access)
        .await
        .map_err(|e| format!("solve failed: {e}"))?;
    let sockets = luma_lib::venue_graph::sockets(&fixtures_root)?;
    let graph = luma_lib::venue_graph::graph(&mut access).await?;

    let fixture_facts: Vec<FixtureFactDump> = solved
        .facts
        .fixtures
        .iter()
        .map(|fact| {
            let row = fixtures.iter().find(|row| row.id == fact.id);
            FixtureFactDump {
                id: fact.id.clone(),
                label: row.and_then(|row| row.label.clone()),
                manufacturer: row.map(|row| row.manufacturer.clone()).unwrap_or_default(),
                model: fact.model.clone(),
                role: fact.role.as_str().to_string(),
                parent: fact.placement.as_ref().map(|p| p.parent.clone()),
                position: fact.placement.as_ref().map(|p| p.position),
            }
        })
        .collect();

    let fixture_ids: std::collections::HashSet<&str> =
        fixtures.iter().map(|row| row.id.as_str()).collect();
    let pieces: Vec<PieceDump> = solved
        .venue
        .poses()
        .filter(|pose| !fixture_ids.contains(pose.node.as_str()))
        .map(|pose| PieceDump {
            node: pose.node.clone(),
            label: pose.label.clone(),
            kind: pose.kind.as_str().to_string(),
            catalog_ref: pose.catalog_ref.clone(),
            parent: pose.parent.clone(),
            position: pose.data_pose().0,
            sockets: graph
                .node(&pose.node)
                .map(|node| sockets.sockets(node))
                .unwrap_or_default()
                .into_iter()
                .map(|socket| SocketDump {
                    name: socket.name,
                    position: data_pose_of_d(
                        pose.world * DMat4::from_translation(DVec3::from(socket.position)),
                    )
                    .0,
                })
                .collect(),
        })
        .collect();

    let dump = VenueDump {
        venue_id: args.venue_id.clone(),
        venue_name,
        fixtures,
        groups,
        fixture_facts,
        pieces,
        stage_centre_x: solved.facts.stage_centre_x,
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
