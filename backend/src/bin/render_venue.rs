//! Render a saved venue without starting a session, refreshing credentials, or
//! writing to the library. Reads the live WAL through SQLite's normal snapshot.
use luma_lib::{
    stage_render::{Shot, VenueGeometry},
    storage::StorageRoot,
};
use luma_scene::View;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut db = StorageRoot::from_env_default()?.luma_db_path();
    let mut venue = None;
    let mut output = None;
    let mut state_path = None;
    let mut view = View::Front;
    let mut format = String::from("png");
    let mut width = 1600u32;
    let mut height = 1000u32;
    while let Some(arg) = args.next() {
        if arg == "--help" {
            println!("render_venue --venue-id UUID --output PATH [--db PATH] [--state PATH] [--format png|catalogue] [--view front|audience|overhead|quarter_left|quarter_right|dj] [--width PX] [--height PX]");
            return Ok(());
        }
        let value = args
            .next()
            .ok_or_else(|| format!("{arg} requires a value"))?;
        match arg.as_str() {
            "--format" => format = value,
            "--db" => db = PathBuf::from(value),
            "--venue-id" => venue = Some(value),
            "--output" => output = Some(PathBuf::from(value)),
            "--state" => state_path = Some(PathBuf::from(value)),
            "--view" => {
                view = value
                    .parse()
                    .map_err(|e: luma_scene::UnknownView| e.to_string())?
            }
            "--width" => width = value.parse().map_err(|_| "width must be an integer")?,
            "--height" => height = value.parse().map_err(|_| "height must be an integer")?,
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    let venue = venue.ok_or("--venue-id is required")?;
    let output = output.ok_or("--output is required")?;
    if !(1..=2000).contains(&width) || !(1..=2000).contains(&height) {
        return Err("width and height must be between 1 and 2000 pixels".into());
    }
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&db).read_only(true))
        .await
        .map_err(|e| format!("{}: {e}", db.display()))?;
    let fixtures_root = luma_lib::headless_host::HostConfig::default().fixtures_root()?;
    let geometry = VenueGeometry::load(&pool, &fixtures_root, &venue).await?;
    let (scene, definitions) = geometry.scene();
    if format == "catalogue" {
        let catalogue = luma_render::scene_desc::Catalogue {
            warmup_frames: 30,
            viewport: luma_render::scene_desc::Viewport { width, height },
            device_scale_factor: 1.0,
            definitions,
            scenes: vec![scene],
        };
        let bytes = serde_json::to_vec_pretty(&catalogue).map_err(|e| e.to_string())?;
        std::fs::write(&output, bytes).map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::json!({"output": output, "venueId": venue, "format": format})
        );
        return Ok(());
    }
    if format != "png" {
        return Err("--format must be png or catalogue".into());
    }

    let environment = geometry.environment;
    let fixture_count = geometry.fixtures.len();
    let state = state_path
        .map(|path| {
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            serde_json::from_slice::<luma_lib::models::universe::UniverseState>(&bytes)
                .map_err(|e| format!("{}: invalid lighting state: {e}", path.display()))
        })
        .transpose()?;
    let shot = Shot {
        view,
        booth: geometry.booth(),
        state,
        time: 0.,
        size: (width, height),
    };
    let meshes = luma_lib::stage_render::meshes_root(Some(&fixtures_root));
    let png = tokio::task::spawn_blocking(move || {
        luma_lib::stage_render::render_png(scene, definitions, shot, meshes)
    })
    .await
    .map_err(|e| format!("render task failed: {e}"))??;
    std::fs::write(&output, png).map_err(|e| format!("{}: {e}", output.display()))?;
    println!(
        "{}",
        serde_json::json!({
            "output": output, "venueId": venue, "view": view.to_string(),
            "width": width, "height": height, "fixtures": fixture_count,
            "environment": environment,
        })
    );
    Ok(())
}
