//! A bounded shared-device benchmark. No GPUI window or display-FPS claim.
use super::*;
use crate::{assets::Library, build_frame, frame::Camera, Catalogue, Renderer};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

fn polled_wait(device: &wgpu::Device, submission: wgpu::SubmissionIndex) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        match device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission.clone()),
            timeout: Some(Duration::ZERO),
        }) {
            Ok(status) if status.wait_finished() => return Ok(()),
            Ok(_) | Err(wgpu::PollError::Timeout) => {}
            Err(error) => return Err(error.into()),
        }
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(30),
            "waveform GPU timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "short native Gasworks stage + waveform A/B diagnostic"]
fn shared_device_stage_and_waveform() -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../harness/perf/mac-gasworks-2026-09-09");
    let output = PathBuf::from(std::env::var_os("LUMA_WAVEFORM_AB_OUTPUT").ok_or_else(|| {
        anyhow::anyhow!("set LUMA_WAVEFORM_AB_OUTPUT to an empty output directory")
    })?);
    std::fs::create_dir_all(&output)?;
    let suite: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("native-suite.json"))?)?;
    let case = suite["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "gasworks-get-lucky-3s-close")
        .unwrap();
    let catalogue = Catalogue::load(&root.join(suite["catalogue"].as_str().unwrap()))?;
    let scene = catalogue
        .scenes
        .iter()
        .find(|s| s.id == case["scene"].as_str().unwrap())
        .unwrap();
    let mut library =
        Library::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"));
    let mut frame = build_frame(scene, &catalogue.definitions, 3.0, &mut library)?;
    frame.haze_resolution = crate::LIVE_HAZE_RESOLUTION;
    frame.camera = Camera {
        eye: glam::Vec3::from_array(serde_json::from_value(case["eye"].clone())?),
        target: glam::Vec3::from_array(serde_json::from_value(case["target"].clone())?),
        fov_y_deg: 50.0,
    };
    let bands: [Vec<f32>; 3] = std::array::from_fn(|band| {
        (0..247 * 6000)
            .map(|i| ((i * (17 + band * 11) % 997) as f32 / 997.) * 0.9)
            .collect()
    });
    let mut runs = Vec::new();
    // Reverse order in the second pair to expose temperature/load drift.
    for (run, blocking) in [true, false, false, true].into_iter().enumerate() {
        let mut renderer = Renderer::new_profiled()?;
        let mut waveform = Waveform::new(
            DeviceContext::shared()?,
            &bands,
            [1.; 3],
            [0.95, 0.8, 0.6],
            6000,
        )?;
        if !blocking {
            waveform.wait_for_gpu = polled_wait;
        }
        for _ in 0..8 {
            renderer.profile_live_frame(&frame, 2227, 1391, crate::LIVE_SUBFRAMES)?;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || -> anyhow::Result<Vec<CpuTimings>> {
            let mut timings = Vec::new();
            let mut count = 0;
            while !worker_stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                let view = View {
                    start: 4.921 + f64::from(count) / 75.,
                    seconds_per_pixel: 1. / 400.,
                    width: 2227,
                    height: 160,
                    padding: 16.,
                    colors: [
                        [0.06, 0.06, 0.07, 1.],
                        [0., 0.333, 0.886, 1.],
                        [0.949, 0.667, 0.235, 1.],
                        [1.; 4],
                    ],
                };
                timings.push(waveform.render(view)?.cpu_timings());
                count += 1;
                std::thread::sleep(
                    Duration::from_secs_f64(1. / 75.).saturating_sub(started.elapsed()),
                );
            }
            Ok(timings)
        });
        let measured = (|| -> anyhow::Result<_> {
            let mut samples = Vec::new();
            for _ in 0..32 {
                let started = Instant::now();
                let timing =
                    renderer.profile_live_frame(&frame, 2227, 1391, crate::LIVE_SUBFRAMES)?;
                samples.push(serde_json::json!({"elapsed_ms": started.elapsed().as_secs_f64() * 1000., "gpu": timing}));
            }
            let pixels = renderer.render_next(&frame, 2227, 1391, crate::LIVE_SUBFRAMES)?;
            Ok((samples, pixels))
        })();
        stop.store(true, Ordering::Relaxed);
        let waveform = worker
            .join()
            .map_err(|_| anyhow::anyhow!("waveform worker panicked"))??;
        let (samples, pixels) = measured?;
        let mode = if blocking { "blocking" } else { "polled" };
        crate::image_out::write(
            &output.join(format!("{run}-{mode}.png")),
            &pixels,
            2227,
            1391,
        )?;
        runs.push(
            serde_json::json!({"run":run, "mode":mode, "stage":samples, "waveform":waveform}),
        );
        std::fs::write(
            output.join("runs.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
            "size":[2227,1391], "stage_time":3.0, "case":case,
            "haze_resolution":frame.haze_resolution, "haze_density":frame.haze_density,
            "fixture_cones":frame.fixture_cones.len(), "live_subframes":crate::LIVE_SUBFRAMES,
                "note":"Pinned Gasworks/Get Lucky stage. Synthetic 247-second 6kHz peak cache with native waveform dimensions and a 75 Hz request ceiling. Same shaders in both modes; only fence wait policy differs. Includes offline profiling overhead; not display FPS or a full app replay.",
                "runs":runs,
            }))?,
        )?;
        eprintln!("shared-device run {run}: {mode}");
    }
    Ok(())
}
