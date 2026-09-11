//! Read-only replay of a saved score through production evaluation and rendering.
//! profile_score SCORE_ID OUTPUT [width height seconds] [all|no-haze|uniform|snapshot]
//! `snapshot` exports frozen renderer scenes at 1, 3 and 7 seconds, without playback.
use luma_lib::{
    eval::{Arena, Scope},
    stage_render::{primitive_state, VenueGeometry},
    storage::StorageRoot,
};
use luma_render::{assets::Library, build_frame_with, Renderer};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::{path::PathBuf, time::Instant};

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let score = args.get(1).ok_or("score id required")?;
    let output = args.get(2).ok_or("output path required")?;
    let number = |i: usize, default: u32| -> Result<u32, String> {
        args.get(i)
            .map(|v| v.parse().map_err(|_| "invalid number".into()))
            .unwrap_or(Ok(default))
    };
    let (width, height, seconds) = (number(3, 1634)?, number(4, 750)?, number(5, 12)?);
    if width == 0 || height == 0 || width > 4096 || height > 4096 || seconds == 0 || seconds > 120 {
        return Err("dimensions must be 1..4096; duration 1..120".into());
    }
    let mode = args.get(6).map(String::as_str).unwrap_or("all");
    if !["all", "no-haze", "uniform", "snapshot"].contains(&mode) {
        return Err("unknown isolation mode".into());
    }
    let storage = StorageRoot::from_env_default()?;
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(storage.luma_db_path())
                .read_only(true),
        )
        .await
        .map_err(|e| e.to_string())?;
    let (venue_id, title, venue_name): (String,String,String) = sqlx::query_as("SELECT s.venue_id,t.title,v.name FROM scores s JOIN tracks t ON t.id=s.track_id JOIN venues v ON v.id=s.venue_id WHERE s.id=?").bind(score).fetch_one(&pool).await.map_err(|e| e.to_string())?;
    let fixtures = luma_lib::headless_host::HostConfig::default().fixtures_root()?;
    let program = luma_lib::build_score_scene(&pool, &storage, &fixtures, score, None).await?;
    let geometry = VenueGeometry::load(&pool, &fixtures, &venue_id).await?;
    let (mut scene, definitions) = geometry.scene();
    scene.render.haze.resolution = luma_render::LIVE_HAZE_RESOLUTION;
    scene.render.haze.steps = 8;
    scene.render.haze.density = 0.24;
    scene.render.show_grid = true;
    scene.render.geometry_shadows = true;
    scene.render.fixture_shadows = true;
    if mode == "no-haze" {
        scene.render.haze.enabled = false;
    }
    if mode == "uniform" {
        scene.render.haze.appearance.cloudiness = 0.0;
    }
    let framing = scene.framing(&definitions);
    let finder = luma_scene::Viewfinder::new(50.0, width as f32 / height as f32)
        .open_air(scene.render.sky.is_some());
    let camera = luma_scene::Camera::for_view(luma_scene::View::Front, &framing, None, &finder);
    scene.camera = luma_render::scene_desc::CameraPose {
        position: luma_render::coords::three_from_world(camera.position()).to_array(),
        target: luma_render::coords::three_from_world(camera.target).to_array(),
    };
    let mut library = Library::new(luma_lib::stage_render::meshes_root(Some(&fixtures)));
    if mode == "snapshot" {
        let mut scenes = Vec::new();
        let mut arena = Arena::default();
        for time in [1.0, 3.0, 7.0] {
            let state = program
                .render(&[time], Scope::Composite, &mut arena)
                .pop()
                .ok_or("missing score frame")?;
            // Record precisely the head keys requested by the production frame
            // builder, including backend single-head fallback resolution.
            let pinned = std::cell::RefCell::new(std::collections::BTreeMap::new());
            build_frame_with(
                &scene,
                &definitions,
                &|id, head| {
                    let value = primitive_state(Some(&state), id, head);
                    if let Some(value) = value {
                        pinned.borrow_mut().insert(format!("{id}:{head}"), value);
                    }
                    value
                },
                time,
                &mut library,
            )
            .map_err(|e| e.to_string())?;
            scene.id = format!("gasworks-get-lucky-{time:.0}s");
            scene.times = vec![time];
            scene.state = pinned.into_inner();
            scenes.push(serde_json::to_value(&scene).map_err(|e| e.to_string())?);
        }
        let artifact = serde_json::json!({"warmupFrames":64,"viewport":{"width":width,"height":height},"deviceScaleFactor":1,"definitions":definitions,"scenes":scenes,"provenance":{"score":score,"title":title,"venue":venue_name,"source":"read-only saved score evaluation"}});
        std::fs::write(
            output,
            serde_json::to_vec_pretty(&artifact).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        println!("{output}");
        return Ok(());
    }
    let mut renderer = Renderer::new_profiled().map_err(|e| e.to_string())?;
    let mut arena = Arena::default();
    let mut rows = Vec::new();
    for i in 0..(seconds * 144 + 60) {
        let time = i.saturating_sub(60) as f32 / 144.0;
        let start = Instant::now();
        let state = program
            .render(&[time], Scope::Composite, &mut arena)
            .pop()
            .ok_or("missing score frame")?;
        let eval_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let mut frame = build_frame_with(
            &scene,
            &definitions,
            &|id, head| primitive_state(Some(&state), id, head),
            time,
            &mut library,
        )
        .map_err(|e| e.to_string())?;
        frame.camera = luma_render::frame::Camera {
            eye: camera.position(),
            target: camera.target,
            fov_y_deg: camera.fov_y_deg,
        };
        let build_ms = start.elapsed().as_secs_f64() * 1000.0;
        let timing = renderer
            .profile_live_frame(&frame, width, height, luma_render::LIVE_SUBFRAMES)
            .map_err(|e| e.to_string())?;
        if i >= 60 {
            rows.push(serde_json::json!({"time":time,"eval_ms":eval_ms,"build_ms":build_ms,"gpu_ms":timing.gpu_total_ms,"scene_ms":timing.gpu_scene_ms,"haze_ms":timing.gpu_volumetric_ms,"grid_ms":timing.gpu_fog_grid_ms,"prepare_ms":timing.gpu_fog_prepare_ms,"lighting_ms":timing.gpu_fog_light_ms,"integrate_ms":timing.gpu_fog_integrate_ms,"composite_ms":timing.gpu_composite_ms,"encode_ms":timing.cpu_encode_submit_ms,"cluster_ms":timing.cpu_cluster_ms,"cones":frame.fixture_cones.len(),"shadow_redraws":renderer.shadow_stats().redrawn_maps}));
        }
    }
    let artifact = serde_json::json!({"score":score,"title":title,"venue":venue_name,"mode":mode,"size":[width,height],"camera":"fitted front; not the live orbit", "haze":scene.render.haze,"adapter":renderer.gpu().adapter_profile().name,"sample_rate":144,"note":"offscreen production score replay; excludes GPUI layout, picking and presentation; GPU timestamps every frame", "frames":rows});
    std::fs::write(
        PathBuf::from(output),
        serde_json::to_vec_pretty(&artifact).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("{output}");
    Ok(())
}
