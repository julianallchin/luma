//! The heads each clip lights on its own venue, for a migration that needs
//! the rig (spec section 0, 2026-09-30 audit: the implied centre of a saved
//! radial or angle space).
//!
//!     clip_cells --db <copy of luma.db>
//!
//! Writes JSON to stdout: `{"domains": [[cell, ...], ...], "clips": {clip
//! id: domain index}}`, one domain per distinct venue, selection and seed.
//! A clip whose cells cannot be resolved maps to its error text. Opens the
//! database read-only.
use luma_lib::database::local::venue_access::{Read, VenueAccess, VenueResource};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::BTreeMap;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut db = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => db = args.next(),
            other => return Err(format!("unknown arg {other}; usage: clip_cells --db PATH")),
        }
    }
    let db = db.ok_or("usage: clip_cells --db PATH")?;
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db)
                .read_only(true)
                .immutable(true),
        )
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let rows: Vec<(String, Option<String>, String, String, Option<String>)> = sqlx::query_as(
        "SELECT c.id, s.venue_id, c.selection_json, c.seed, c.selection_seed
         FROM clips c JOIN scores s ON s.id = c.score_id ORDER BY c.id",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("clips query failed: {e}"))?;
    let fixtures_root = luma_lib::services::fixtures::resolve_fixtures_root_from(None)?;
    let mut keys: BTreeMap<(String, String, u64), serde_json::Value> = BTreeMap::new();
    let mut domains = Vec::new();
    let mut clips = serde_json::Map::new();
    for (id, venue, selection, seed, selection_seed) in rows {
        let Some(venue) = venue else {
            clips.insert(id, "the score has no venue".into());
            continue;
        };
        let seed: u64 = selection_seed
            .as_deref()
            .unwrap_or(&seed)
            .parse()
            .map_err(|e| format!("clip {id}: seed: {e}"))?;
        let key = (venue.clone(), selection.clone(), seed);
        if let Some(known) = keys.get(&key) {
            clips.insert(id, known.clone());
            continue;
        }
        let parsed: luma_patterns::Selection =
            serde_json::from_str(&selection).map_err(|e| format!("clip {id}: {e}"))?;
        let mut access = VenueAccess::<Read>::read(&pool, VenueResource::Venue(&venue))
            .await
            .map_err(|e| format!("venue {venue} not readable: {e}"))?;
        let answer = match luma_lib::services::composable_patterns::resolve_cells(
            &mut access,
            &fixtures_root,
            std::slice::from_ref(&parsed),
            seed,
        )
        .await
        {
            Ok(cells) => {
                domains.push(serde_json::to_value(cells).map_err(|e| e.to_string())?);
                serde_json::Value::from(domains.len() - 1)
            }
            Err(error) => serde_json::Value::from(error),
        };
        keys.insert(key, answer.clone());
        clips.insert(id, answer);
    }
    println!(
        "{}",
        serde_json::json!({"domains": domains, "clips": clips})
    );
    Ok(())
}
