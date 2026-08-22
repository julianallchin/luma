//! Acceptance profiler for production volumetric rendering.
//!
//! Run from `gpui/` with `cargo run -p luma-render --release --bin
//! profile-volumetrics`. Hardware pass-boundary timestamps measure GPU work;
//! CPU encode time ends at queue submission and excludes waits/readback.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::frame::FixtureCone;
use luma_render::scene_desc::{CameraPose, DebugView, Environment, RenderSettings, Scene};
use luma_render::{build_frame_with, FrameTimings, Renderer, LIVE_SUBFRAMES};
use serde::Serialize;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const WARMUP_FRAMES: usize = 120;
const MEASURED_FRAMES: usize = 600;
const PROFILE_ARTIFACT: &str = "gpui/crates/render/goldens/volumetric-profile-m3-max.json";
const PROVENANCE_SCOPES: [&str; 2] = ["gpui/crates/render", "gpui/crates/app"];

#[derive(Serialize)]
struct MetricSummary {
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

#[derive(Serialize)]
struct CaseResult {
    cones: usize,
    samples: usize,
    samples_fnv64: String,
    gpu_total: MetricSummary,
    gpu_volumetric: MetricSummary,
    cpu_encode_submit: MetricSummary,
    budgets_ms: serde_json::Value,
    within_budget: bool,
}

#[derive(Serialize)]
struct ProvenanceEntry {
    path: String,
    state: &'static str,
    bytes: Option<usize>,
    content_fnv64: Option<String>,
}

#[derive(Serialize)]
struct SourceProvenance {
    algorithm: &'static str,
    scopes: [&'static str; 2],
    extensions: [&'static str; 4],
    excluded: [&'static str; 2],
    entries: Vec<ProvenanceEntry>,
    manifest_fnv64: String,
}

fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        !cfg!(debug_assertions),
        "acceptance timings require a --release build"
    );
    let arguments: Vec<_> = std::env::args().collect();
    let smoke_256 = arguments.iter().any(|argument| argument == "--smoke-256");
    let smoke = smoke_256 || arguments.iter().any(|argument| argument == "--smoke");
    let warmup_frames = if smoke { 2 } else { WARMUP_FRAMES };
    let measured_frames = if smoke { 20 } else { MEASURED_FRAMES };
    let base = base_frame()?;
    let mut renderer = Renderer::new_profiled()?;
    let adapter = renderer.adapter_profile().clone();
    let first_cones = if smoke_256 { 256 } else { 64 };
    let mut cases = vec![profile_case(
        &mut renderer,
        &base,
        first_cones,
        if smoke_256 { 6.5 } else { 8.0 },
        smoke_256.then_some(3.0),
        1.5,
        warmup_frames,
        measured_frames,
    )?];
    if !smoke {
        cases.push(profile_case(
            &mut renderer,
            &base,
            256,
            6.5,
            Some(3.0),
            1.5,
            warmup_frames,
            measured_frames,
        )?);
    }
    let all_pass = cases.iter().all(|case| case.within_budget);
    let repository = git_repository_root()?;
    let status = checked_command_text(Command::new("git").current_dir(&repository).args([
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
    ]))?;
    let provenance = source_provenance(&repository)?;
    let artifact = serde_json::json!({
        "schema": "luma.renderer-profile/2",
        "captured_at_utc": command_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        "source": {
            "commit": checked_git_text(&repository, &["rev-parse", "HEAD"] )?,
            "branch": checked_git_text(&repository, &["branch", "--show-current"] )?,
            "dirty": !status.is_empty(),
            "provenance": provenance,
        },
        "build": {
            "profile": "release",
            "debug_assertions": cfg!(debug_assertions),
            "target": rustc_host()?,
            "arch": std::env::consts::ARCH,
            "rustc": checked_command_text(Command::new("rustc").arg("--version"))?,
            "crate_version": env!("CARGO_PKG_VERSION"),
            "wgpu_lock": "26.0.1",
        },
        "host": {
            "os": std::env::consts::OS,
            "os_version": os_version(),
            "arch": std::env::consts::ARCH,
        },
        "adapter": {
            "name": adapter.name,
            "backend": adapter.backend,
            "device_type": adapter.device_type,
            "driver": adapter.driver,
            "driver_info": adapter.driver_info,
            "timestamp_query_supported": adapter.timestamp_query_supported,
            "timestamp_period_ns": adapter.timestamp_period_ns,
        },
        "display": {
            "scale_factor": 1.0,
            "interpretation": "offscreen physical pixels",
        },
        "repro": {
            "command": if smoke_256 {
                "cargo run -p luma-render --release --bin profile-volumetrics -- --smoke-256"
            } else if smoke {
                "cargo run -p luma-render --release --bin profile-volumetrics -- --smoke"
            } else {
                "cargo run -p luma-render --release --bin profile-volumetrics"
            },
            "cwd": "gpui",
            "width": WIDTH,
            "height": HEIGHT,
            "haze_resolution": 0.5,
            "haze_steps": 8,
            "subframes": LIVE_SUBFRAMES,
            "temporal": true,
            "debug_view": "pbr",
            "warmup_frames": warmup_frames,
            "measured_frames": measured_frames,
            "time_step_seconds": 1.0 / 60.0,
            "percentile": "nearest-rank ceil(q*n)-1",
        },
        "cases": cases,
        "all_within_budget": all_pass,
    });
    println!("{}", serde_json::to_string_pretty(&artifact)?);
    anyhow::ensure!(all_pass, "one or more renderer acceptance budgets failed");
    Ok(())
}

fn profile_case(
    renderer: &mut Renderer,
    base: &luma_render::Frame,
    cones: usize,
    total_budget_ms: f64,
    volumetric_budget_ms: Option<f64>,
    cpu_budget_ms: f64,
    warmup_frames: usize,
    measured_frames: usize,
) -> anyhow::Result<CaseResult> {
    let mut frame = frame_with_lights(base, cones);
    for sample in 0..warmup_frames {
        frame.time = sample as f32 / 60.0;
        renderer.profile_live_frame(&frame, WIDTH, HEIGHT, LIVE_SUBFRAMES)?;
    }
    let mut samples = Vec::with_capacity(measured_frames);
    for sample in 0..measured_frames {
        frame.time = (warmup_frames + sample) as f32 / 60.0;
        samples.push(renderer.profile_live_frame(&frame, WIDTH, HEIGHT, LIVE_SUBFRAMES)?);
    }
    let total = summarize(samples.iter().map(|sample| sample.gpu_total_ms));
    let volumetric = summarize(samples.iter().map(|sample| sample.gpu_volumetric_ms));
    let cpu = summarize(samples.iter().map(|sample| sample.cpu_encode_submit_ms));
    let within_budget = total.p95_ms <= total_budget_ms
        && cpu.p95_ms <= cpu_budget_ms
        && volumetric_budget_ms.is_none_or(|budget| volumetric.p95_ms <= budget);
    let mut sample_bytes = Vec::with_capacity(samples.len() * 24);
    for FrameTimings {
        gpu_total_ms,
        gpu_volumetric_ms,
        cpu_encode_submit_ms,
    } in &samples
    {
        sample_bytes.extend_from_slice(&gpu_total_ms.to_bits().to_le_bytes());
        sample_bytes.extend_from_slice(&gpu_volumetric_ms.to_bits().to_le_bytes());
        sample_bytes.extend_from_slice(&cpu_encode_submit_ms.to_bits().to_le_bytes());
    }
    Ok(CaseResult {
        cones,
        samples: samples.len(),
        samples_fnv64: format!("0x{:016x}", fnv64(&sample_bytes)),
        gpu_total: total,
        gpu_volumetric: volumetric,
        cpu_encode_submit: cpu,
        budgets_ms: serde_json::json!({
            "gpu_total_p95": total_budget_ms,
            "gpu_volumetric_p95": volumetric_budget_ms,
            "cpu_encode_submit_p95": cpu_budget_ms,
        }),
        within_budget,
    })
}

fn summarize(samples: impl Iterator<Item = f64>) -> MetricSummary {
    let mut samples: Vec<_> = samples.collect();
    samples.sort_by(f64::total_cmp);
    let rank = |quantile: f64| {
        ((quantile * samples.len() as f64).ceil() as usize - 1).min(samples.len() - 1)
    };
    MetricSummary {
        p50_ms: samples[rank(0.50)],
        p95_ms: samples[rank(0.95)],
        max_ms: *samples.last().expect("the profiler always records samples"),
    }
}

fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn git_repository_root() -> anyhow::Result<PathBuf> {
    let root = checked_command_text(Command::new("git").args(["rev-parse", "--show-toplevel"]))?;
    let root = PathBuf::from(root);
    anyhow::ensure!(
        root.is_absolute(),
        "Git returned a non-absolute repository root"
    );
    Ok(root)
}

fn checked_git_text(repository: &Path, args: &[&str]) -> anyhow::Result<String> {
    checked_command_text(Command::new("git").current_dir(repository).args(args))
}

fn checked_git_bytes(repository: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    checked_command(Command::new("git").current_dir(repository).args(args))
        .map(|output| output.stdout)
}

fn checked_command(command: &mut Command) -> anyhow::Result<Output> {
    let debug = format!("{command:?}");
    let output = command
        .output()
        .map_err(|error| anyhow::anyhow!("failed to execute {debug}: {error}"))?;
    anyhow::ensure!(
        output.status.success(),
        "{debug} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output)
}

fn checked_command_text(command: &mut Command) -> anyhow::Result<String> {
    let output = checked_command(command)?;
    let text = String::from_utf8(output.stdout)
        .map_err(|error| anyhow::anyhow!("command output was not UTF-8: {error}"))?;
    Ok(text.trim().to_owned())
}

fn rustc_host() -> anyhow::Result<String> {
    let verbose = checked_command_text(Command::new("rustc").arg("-vV"))?;
    verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("rustc -vV did not report a host target"))
}

fn source_provenance(repository: &Path) -> anyhow::Result<SourceProvenance> {
    let mut tracked_args = vec![
        "diff",
        "--name-only",
        "--no-renames",
        "--diff-filter=ACMRTUXBD",
        "-z",
        "HEAD",
        "--",
    ];
    tracked_args.extend(PROVENANCE_SCOPES);
    let tracked = nul_paths(&checked_git_bytes(repository, &tracked_args)?)?;

    let mut untracked_args = vec!["ls-files", "--others", "--exclude-standard", "-z", "--"];
    untracked_args.extend(PROVENANCE_SCOPES);
    let untracked = nul_paths(&checked_git_bytes(repository, &untracked_args)?)?;

    let tracked: BTreeSet<_> = tracked
        .into_iter()
        .filter(|path| relevant_path(path))
        .collect();
    let untracked: BTreeSet<_> = untracked
        .into_iter()
        .filter(|path| relevant_path(path))
        .collect();
    anyhow::ensure!(
        tracked.is_disjoint(&untracked),
        "Git reported a path as both tracked-dirty and untracked"
    );

    let mut entries = Vec::with_capacity(tracked.len() + untracked.len());
    for (path, state) in tracked
        .iter()
        .map(|path| (path, "tracked"))
        .chain(untracked.iter().map(|path| (path, "untracked")))
    {
        let absolute = repository.join(path);
        match fs::read(&absolute) {
            Ok(content) => entries.push(ProvenanceEntry {
                path: path.clone(),
                state,
                bytes: Some(content.len()),
                content_fnv64: Some(format!("0x{:016x}", fnv64(&content))),
            }),
            Err(error) if state == "tracked" && error.kind() == std::io::ErrorKind::NotFound => {
                entries.push(ProvenanceEntry {
                    path: path.clone(),
                    state: "deleted",
                    bytes: None,
                    content_fnv64: None,
                });
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "cannot fingerprint relevant {state} path {path}: {error}"
                ));
            }
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest_fnv64 = manifest_hash(&entries);
    Ok(SourceProvenance {
        algorithm: "fnv1a64(length(path),path,length(state),state,content_length,length(content_fnv64),content_fnv64)",
        scopes: PROVENANCE_SCOPES,
        extensions: ["rs", "wgsl", "json", "png"],
        excluded: [PROFILE_ARTIFACT, ".husky/**"],
        entries,
        manifest_fnv64: format!("0x{manifest_fnv64:016x}"),
    })
}

fn nul_paths(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path)
                .map_err(|error| anyhow::anyhow!("Git path was not UTF-8: {error}"))?;
            let safe = Path::new(path)
                .components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
            anyhow::ensure!(safe, "Git returned unsafe provenance path {path}");
            Ok(path.to_owned())
        })
        .collect()
}

fn relevant_path(path: &str) -> bool {
    path != PROFILE_ARTIFACT
        && !path.split('/').any(|component| component == ".husky")
        && matches!(
            Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("rs" | "wgsl" | "json" | "png")
        )
}

fn manifest_hash(entries: &[ProvenanceEntry]) -> u64 {
    let mut bytes = Vec::new();
    for entry in entries {
        append_field(&mut bytes, entry.path.as_bytes());
        append_field(&mut bytes, entry.state.as_bytes());
        match (entry.bytes, entry.content_fnv64.as_deref()) {
            (Some(length), Some(hash)) => {
                bytes.extend_from_slice(&(length as u64).to_le_bytes());
                append_field(&mut bytes, hash.as_bytes());
            }
            (None, None) => bytes.extend_from_slice(&u64::MAX.to_le_bytes()),
            _ => unreachable!("manifest entry content fields move together"),
        }
    }
    fnv64(&bytes)
}

fn append_field(output: &mut Vec<u8>, field: &[u8]) {
    output.extend_from_slice(&(field.len() as u64).to_le_bytes());
    output.extend_from_slice(field);
}

#[cfg(test)]
mod provenance_tests {
    use super::{manifest_hash, relevant_path, ProvenanceEntry, PROFILE_ARTIFACT};

    fn entry(path: &str, hash: &str) -> ProvenanceEntry {
        ProvenanceEntry {
            path: path.into(),
            state: "untracked",
            bytes: Some(4),
            content_fnv64: Some(hash.into()),
        }
    }

    #[test]
    fn provenance_scope_accepts_evidence_and_excludes_its_output() {
        assert!(relevant_path("gpui/crates/render/src/gpu.rs"));
        assert!(relevant_path(
            "gpui/crates/render/goldens/volumetric-stress-32.png"
        ));
        assert!(!relevant_path(PROFILE_ARTIFACT));
        assert!(!relevant_path(".husky/_/husky.sh"));
        assert!(!relevant_path("gpui/crates/render/target/cache.bin"));
    }

    #[test]
    fn manifest_hash_binds_path_state_length_and_content_hash() {
        let original = [entry("gpui/crates/render/src/a.rs", "0x0000000000000001")];
        let changed_content = [entry("gpui/crates/render/src/a.rs", "0x0000000000000002")];
        let changed_path = [entry("gpui/crates/render/src/b.rs", "0x0000000000000001")];
        let mut changed_state = entry("gpui/crates/render/src/a.rs", "0x0000000000000001");
        changed_state.state = "tracked";
        let changed_state = [changed_state];
        assert_ne!(manifest_hash(&original), manifest_hash(&changed_content));
        assert_ne!(manifest_hash(&original), manifest_hash(&changed_path));
        assert_ne!(manifest_hash(&original), manifest_hash(&changed_state));
    }
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".into())
}

fn os_version() -> String {
    if cfg!(target_os = "macos") {
        command_output("sw_vers", &["-productVersion"])
    } else {
        command_output("uname", &["-sr"])
    }
}

fn base_frame() -> anyhow::Result<luma_render::Frame> {
    let mut settings = RenderSettings::dark_stage(48.0, 0.5);
    settings.environment = Environment::DARK;
    settings.haze.enabled = true;
    settings.haze.steps = 8;
    settings.haze.density = 0.65;
    settings.debug_view = DebugView::Pbr;
    let scene = Scene {
        id: "volumetric-profile".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [4.5, 3.0, 5.0],
            target: [0.0, 0.8, 0.0],
        },
        editing: false,
        render: settings,
        selected_fixture_ids: Vec::new(),
        fixtures: Vec::new(),
        pieces: Vec::new(),
        state: BTreeMap::new(),
    };
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library)
}

fn frame_with_lights(base: &luma_render::Frame, count: usize) -> luma_render::Frame {
    let mut frame = luma_render::Frame {
        meshes: base
            .meshes
            .iter()
            .map(|mesh| luma_render::frame::MeshData {
                key: mesh.key.clone(),
                vertices: mesh.vertices.clone(),
                indices: mesh.indices.clone(),
            })
            .collect(),
        images: Vec::new(),
        draws: base
            .draws
            .iter()
            .map(|draw| luma_render::frame::Draw {
                mesh: draw.mesh,
                model: draw.model,
                material: draw.material,
                textures: draw.textures,
            })
            .collect(),
        grid_draws: base.grid_draws,
        overlays: Vec::new(),
        point_lights: base.point_lights.clone(),
        fixture_cones: Vec::with_capacity(count),
        clear_color: base.clear_color,
        ambient: base.ambient,
        directional: base.directional,
        haze_density: base.haze_density,
        haze_steps: base.haze_steps,
        haze_resolution: base.haze_resolution,
        time: 0.0,
        debug_view: base.debug_view,
        camera: base.camera,
    };
    for index in 0..count {
        let column = (index % 32) as f32;
        let row = (index / 32) as f32;
        frame.fixture_cones.push(FixtureCone {
            position: Vec3::new((column - 15.5) * 0.18, (row - 7.5) * 0.18, 0.15),
            range: 8.0,
            direction: Vec3::Z,
            cos_beam: 0.975,
            color: Vec3::new(0.2, 0.55, 1.0),
            intensity: 0.01,
            cos_field: 0.93,
            wash: 0.0,
            gobo: (index % 3) as u32,
            gobo_rotation: 0.31,
        });
    }
    frame
}
