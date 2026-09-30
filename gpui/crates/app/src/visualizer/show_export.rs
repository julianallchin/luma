//! "Export show": the whole score, rendered offscreen to an mp4.
//!
//! Frame `i` is the score at `i / FPS` seconds, from 0 to the end — never the
//! wall clock, so the file does not depend on how fast the GPU was. Nothing
//! waits for a display either: [`Recorder`] draws as fast as the GPU goes,
//! with its readbacks pipelined, and [`Encoder`] takes the frames on a thread
//! of its own.
//!
//! The frames are the viewport's. The rig, the render settings, the score
//! sample ([`Library::score_sampler`](crate::Library::score_sampler)), the
//! motor lag, the footage look and the live pass chain are the ones the stage
//! draws with. Only the camera differs: it is the stage's camera when the
//! export starts, and it stays there. And with the footage look on, a frame's
//! shutter holds [`SHUTTER_SUBFRAMES`] moments where the stage affords
//! [`luma_render::LIVE_SUBFRAMES`].

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::watch;

use luma_lib::models::universe::UniverseState;
use luma_lib::recording::{Encode, Encoder};
use luma_lib::stage_render;
use luma_render::{assets, build_frame_at, coords, scene_desc, Frame, Recorder};

use super::motors::Motors;
use super::{Bass, Visualizer};

/// Output frames per second.
pub(crate) const FPS: u32 = 60;

/// Moments in one frame's shutter with the footage look on. The export is not
/// paced to a display, so it spends what the stage cannot: each is a whole
/// render.
const SHUTTER_SUBFRAMES: u32 = 8;

/// Frames drawn and thrown away before score time 0, so the haze history has
/// settled by the first frame kept. The temporal resolve keeps 82% of its
/// history, so the first frame's weight is under 1% after 24.
const WARMUP: u64 = 24;

/// The stage as the viewport has it: the rig, its render settings and the
/// camera. Owned, so the export does not follow later changes to the view.
pub(crate) struct Shot {
    scene: scene_desc::Scene,
    definitions: std::collections::BTreeMap<String, scene_desc::Definition>,
    /// The viewport's width over its height.
    pub(crate) aspect: f32,
    /// The track's bass, for the footage look's shake.
    bass: Option<Bass>,
}

impl Visualizer {
    /// The stage now, for an export. `None` before the rig has landed or the
    /// pane has been laid out.
    ///
    /// A show, not the editor: the grid, the gizmos and the selection are
    /// left out.
    pub(crate) fn shot(&self) -> Option<Shot> {
        let (width, height) = (f32::from(self.size.width), f32::from(self.size.height));
        if width < 1.0 || height < 1.0 {
            return None;
        }
        let stage = self.stage.borrow();
        let mut scene = stage.scene.clone()?;
        let camera = self.camera;
        scene.camera.position = coords::three_from_world(camera.position()).to_array();
        scene.camera.target = coords::three_from_world(camera.target).to_array();
        scene.render = self.render_controls.settings(camera.fov_y_deg);
        scene.render.show_grid = false;
        scene.render.show_gizmos = false;
        scene.editing = false;
        scene.selected_fixture_ids.clear();
        scene.editor = scene_desc::Editor::default();
        Some(Shot {
            scene,
            definitions: stage.definitions.clone(),
            aspect: width / height,
            bass: self.bass.clone(),
        })
    }

    /// Whether the rig is lit by the score, so an export has something to
    /// show.
    pub(crate) fn is_lit(&self) -> bool {
        self.lit.is_some()
    }
}

/// The largest frame of `aspect` that fits in `frame`, with even sides, as
/// yuv420p needs.
pub(crate) fn fit(frame: (u32, u32), aspect: f32) -> (u32, u32) {
    let (width, height) = (frame.0 as f32, frame.1 as f32);
    let (width, height) = if aspect >= width / height {
        (width, width / aspect)
    } else {
        (height * aspect, height)
    };
    let even = |side: f32| ((side / 2.0).round() as u32 * 2).max(2);
    (even(width), even(height))
}

/// Output frames that cover `duration` seconds: the last one starts before
/// the end. A frame that would start within a thousandth of a frame of the
/// end is float noise in the duration, not a frame.
pub(crate) fn frame_count(duration: f32) -> u64 {
    (f64::from(duration.max(0.0)) * f64::from(FPS) - 1e-3)
        .ceil()
        .max(0.0) as u64
}

/// One step of the export's clock.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tick {
    /// The renderer's and the motors' clock, in seconds: the air drifts and
    /// the strobes flash on it. It runs through the warm-up, so both see one
    /// continuous sequence.
    clock: f64,
    /// Score time, in seconds. 0 through the warm-up.
    score: f32,
    /// Whether this frame goes into the file.
    kept: bool,
}

/// [`WARMUP`] ticks at score time 0, then one tick per output frame, `1 / FPS`
/// apart.
fn ticks(duration: f32) -> impl Iterator<Item = Tick> {
    let frames = frame_count(duration);
    (0..WARMUP + frames).map(|n| {
        let kept = n >= WARMUP;
        let frame = n.saturating_sub(WARMUP);
        Tick {
            clock: n as f64 / f64::from(FPS),
            score: (frame as f64 / f64::from(FPS)) as f32,
            kept,
        }
    })
}

/// Everything one export needs, captured when Start is pressed.
pub(crate) struct Job {
    pub(crate) shot: Shot,
    /// Output pixels.
    pub(crate) size: (u32, u32),
    /// The score's length, in seconds.
    pub(crate) duration: f32,
    pub(crate) sample: Box<dyn Fn(f32) -> Option<UniverseState> + Send>,
    /// The track's audio file, when it is on this device.
    pub(crate) audio: Option<PathBuf>,
    pub(crate) output: PathBuf,
}

/// Where an export has got to.
#[derive(Clone, Debug)]
pub(crate) struct Progress {
    /// Frames in the file so far.
    pub(crate) done: u64,
    pub(crate) total: u64,
    pub(crate) started: Instant,
    /// `None` while it runs.
    pub(crate) outcome: Option<Result<(), String>>,
}

/// How often the export thread publishes its progress. The finish is
/// published whenever it comes.
const REPORT: std::time::Duration = std::time::Duration::from_millis(100);

/// A running export. Dropping it cancels: rendering stops at the next frame,
/// ffmpeg is killed and the partial file deleted.
pub(crate) struct Export {
    progress: watch::Receiver<Progress>,
    cancel: Arc<AtomicBool>,
}

impl Export {
    /// Start rendering `job` on a thread of its own.
    pub(crate) fn start(job: Job) -> Self {
        let (report, progress) = watch::channel(Progress {
            done: 0,
            total: frame_count(job.duration),
            started: Instant::now(),
            outcome: None,
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("luma-show-export".into())
            .spawn(move || {
                let outcome = run(job, &stop, &report);
                report.send_modify(|progress| {
                    let elapsed = progress.started.elapsed().as_secs_f64();
                    eprintln!(
                        "show export: {} of {} frames in {elapsed:.1} s, {:.1} fps: {outcome:?}",
                        progress.done,
                        progress.total,
                        progress.done as f64 / elapsed.max(1e-3),
                    );
                    progress.outcome = Some(outcome);
                });
            })
            .expect("the export thread could not be started");
        Self { progress, cancel }
    }

    pub(crate) fn progress(&self) -> Progress {
        self.progress.borrow().clone()
    }

    /// Wakes when the progress has moved since it was last read. Owned, so
    /// the dialog can wait on it without holding the export.
    pub(crate) fn updates(&self) -> watch::Receiver<Progress> {
        self.progress.clone()
    }
}

impl Drop for Export {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The frame loop. Blocks for the length of the render.
fn run(job: Job, cancel: &AtomicBool, report: &watch::Sender<Progress>) -> Result<(), String> {
    let Job {
        shot,
        size: (width, height),
        duration,
        sample,
        audio,
        output,
    } = job;
    let seconds = frame_count(duration) as f32 / FPS as f32;
    let mut encoder = Encoder::start(&Encode {
        size: (width, height),
        fps: FPS,
        audio: audio.as_deref().map(|path| (path, 0.0, seconds)),
        output: &output,
    })
    .map_err(|error| error.to_string())?;
    let mut recorder =
        Recorder::new().map_err(|error| format!("No GPU for the export: {error}"))?;
    let mut assets = assets::Library::new(stage_render::meshes_root(None));
    let mut motors = Motors::default();
    // Whether each frame in flight is kept, oldest first. The recorder hands
    // frames back in the order they went in.
    let mut in_flight = VecDeque::new();
    let (mut done, mut reported) = (0, Instant::now());
    let mut keep = |kept: Option<bool>, pixels: Vec<u8>| -> Result<(), String> {
        if kept == Some(true) {
            encoder.write(pixels).map_err(|error| error.to_string())?;
            done += 1;
            if reported.elapsed() >= REPORT {
                reported = Instant::now();
                report.send_modify(|progress| progress.done = done);
            }
        }
        Ok(())
    };
    for tick in ticks(duration) {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        // The shutter closes on the tick. Track time runs with the clock once
        // the score has started, and holds at 0 through the warm-up.
        let transport = |at: f64| {
            if tick.kept {
                (f64::from(tick.score) - (tick.clock - at)).max(0.0) as f32
            } else {
                tick.score
            }
        };
        let moments = luma_render::footage::moments(
            &shot.scene.render.look.footage,
            tick.clock,
            1.0 / f64::from(FPS),
            SHUTTER_SUBFRAMES,
            |at| {
                shot.bass
                    .as_ref()
                    .map_or(0.0, |bass| bass.at(transport(at)))
            },
        );
        let mut frames = Vec::with_capacity(moments.len());
        for moment in moments {
            let at = transport(moment.time);
            let universe = motors.step(moment.time, at, sample(at));
            frames.push(
                build_frame_at(
                    &shot.scene,
                    &shot.definitions,
                    &|id, head| stage_render::primitive_state(universe.as_ref(), id, head),
                    moment,
                    &mut assets,
                )
                .map_err(|error| format!("Could not assemble the frame: {error}"))?,
            );
        }
        let frame = Frame::exposure(frames);
        in_flight.push_back(tick.kept);
        if let Some(pixels) = recorder
            .push(&frame, width, height)
            .map_err(|error| format!("Could not render the frame: {error}"))?
        {
            keep(in_flight.pop_front(), pixels)?;
        }
    }
    while let Some(pixels) = recorder
        .pop()
        .map_err(|error| format!("Could not render the frame: {error}"))?
    {
        keep(in_flight.pop_front(), pixels)?;
    }
    drop(keep);
    report.send_modify(|progress| progress.done = done);
    encoder.finish().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_of_the_score_is_kept_once_at_its_own_time() {
        for duration in [0.5_f32, 3.33, 10.0, 10.01] {
            let kept: Vec<Tick> = ticks(duration).filter(|tick| tick.kept).collect();
            assert_eq!(
                kept.len() as f64,
                (f64::from(duration) * 60.0).ceil(),
                "{duration} s"
            );
            for (i, tick) in kept.iter().enumerate() {
                assert_eq!(tick.score, (i as f64 / 60.0) as f32);
            }
            assert!(kept.last().unwrap().score < duration);
        }
    }

    #[test]
    fn the_clock_steps_evenly_through_the_warm_up_into_the_score() {
        let all: Vec<Tick> = ticks(1.0).collect();
        let first = all.iter().position(|tick| tick.kept).unwrap();
        assert!(all[..first].iter().all(|tick| tick.score == 0.0));
        assert!(all[first..].iter().all(|tick| tick.kept));
        for pair in all.windows(2) {
            assert!((pair[1].clock - pair[0].clock - 1.0 / 60.0).abs() < 1e-9);
        }
    }

    #[test]
    fn the_frame_keeps_the_viewport_shape_inside_the_preset() {
        let (width, height) = fit((3840, 2160), 3.2);
        assert!(width <= 3840 && height <= 2160);
        assert!(((width as f32 / height as f32) - 3.2).abs() < 0.01);
        assert_eq!((width % 2, height % 2), (0, 0));
        let (width, height) = fit((1920, 1080), 0.5);
        assert_eq!(height, 1080);
        assert!(width < 1920);
    }
}
