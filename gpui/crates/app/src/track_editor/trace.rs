//! Opt-in, bounded timeline trace. No per-frame file I/O or unbounded logging.
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Instant,
};

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    Frame {
        ms: f64,
        scroll_px: f32,
        zoom: f32,
        position_s: f32,
        paint_ms: f64,
        window_active: bool,
        waveform_updates_frozen: bool,
    },
    Window {
        ms: f64,
        phase: &'static str,
        cpu_ms: f64,
    },
    Waveform {
        ms: f64,
        overview: bool,
        queue_ms: f64,
        render_ms: f64,
        publish_ms: f64,
        cpu: Option<luma_render::waveform::CpuTimings>,
    },
}
struct Capture {
    path: PathBuf,
    start: Option<Instant>,
    events: Vec<Event>,
    done: bool,
    duration_ms: f64,
}
fn capture() -> Option<&'static Mutex<Capture>> {
    static CAPTURE: OnceLock<Option<Mutex<Capture>>> = OnceLock::new();
    CAPTURE
        .get_or_init(|| {
            std::env::var_os("LUMA_TIMELINE_TRACE").map(|path| {
                Mutex::new(Capture {
                    path: path.into(),
                    start: None,
                    events: Vec::with_capacity(12000),
                    done: false,
                    duration_ms: std::env::var("LUMA_TIMELINE_TRACE_SECONDS")
                        .ok()
                        .and_then(|v| v.parse::<u32>().ok())
                        .map_or(30, |v| v.clamp(1, 30)) as f64
                        * 1000.,
                })
            })
        })
        .as_ref()
}
pub(super) fn frame(
    active: bool,
    started: Instant,
    scroll: f32,
    zoom: f32,
    position: f32,
    window_active: bool,
) {
    let Some(capture) = capture() else {
        return;
    };
    let mut capture = capture.lock().unwrap();
    if capture.done || !active {
        return;
    }
    let duration_ms = capture.duration_ms;
    let origin = *capture.start.get_or_insert_with(|| {
        gpui::profiler::start_frame_trace();
        eprintln!(
            "[timeline trace] recording {} seconds of focus playback",
            duration_ms / 1000.
        );
        started
    });
    let ms = started.duration_since(origin).as_secs_f64() * 1000.;
    capture.events.push(Event::Frame {
        ms,
        window_active,
        waveform_updates_frozen: super::waveform::updates_frozen(),
        scroll_px: scroll,
        zoom,
        position_s: position,
        paint_ms: started.elapsed().as_secs_f64() * 1000.,
    });
    if ms >= capture.duration_ms || capture.events.len() >= 12000 {
        capture.done = true;
        let mut events = std::mem::take(&mut capture.events);
        events.extend(
            gpui::profiler::take_frame_trace()
                .into_iter()
                .map(|(at, phase, cpu_ms)| Event::Window {
                    ms: at.saturating_duration_since(origin).as_secs_f64() * 1000.,
                    phase,
                    cpu_ms,
                }),
        );
        let path = capture.path.clone();
        std::thread::spawn(move || {
            let result = std::fs::File::create(&path)
                .map_err(|e| e.to_string())
                .and_then(|file| {
                    serde_json::to_writer(std::io::BufWriter::new(file), &events)
                        .map_err(|e| e.to_string())
                });
            eprintln!("[timeline trace] {}: {:?}", path.display(), result);
        });
    }
}
pub(super) fn waveform(
    overview: bool,
    queued: Instant,
    started: Instant,
    finished: Instant,
    cpu: Option<luma_render::waveform::CpuTimings>,
) {
    let Some(capture) = capture() else {
        return;
    };
    let mut capture = capture.lock().unwrap();
    if capture.done {
        return;
    }
    let Some(origin) = capture.start else {
        return;
    };
    let now = Instant::now();
    capture.events.push(Event::Waveform {
        ms: now.duration_since(origin).as_secs_f64() * 1000.,
        overview,
        queue_ms: started.duration_since(queued).as_secs_f64() * 1000.,
        render_ms: finished.duration_since(started).as_secs_f64() * 1000.,
        publish_ms: now.duration_since(finished).as_secs_f64() * 1000.,
        cpu,
    });
}
