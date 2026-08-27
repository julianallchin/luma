//! One score, rendered to an mp4 with the track's own audio.
//!
//! This module owns the *time axis* and the *container*. `stage_render` is one
//! venue at one moment; recording is a grid of moments and a file format, which
//! is a different abstraction and therefore a different module. Everything a
//! caller would otherwise re-derive — span clamping, frame count, shutter,
//! encoder choice, the audio offset, the ffmpeg argv — lives here exactly once,
//! so the CLI, and any later app dialog, only start a recording and wait.
//!
//! The whole performance argument is that scene assembly happens *once*.
//! `venue.render`'s ~150 ms per still is almost all `VenueGeometry::load` +
//! `build_scene_strict` + a PNG deflate; a recording pays those on frame zero
//! and then per frame carries only `(light state, t)` through
//! [`stage_render::Sequence`], with RGBA going straight into the pipe.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::SqlitePool;

use crate::eval::{Arena, Scope};
use crate::models::scores::TrackScore;
use crate::models::universe::UniverseState;
use crate::stage_render::{self, Sequence, VenueGeometry};
use crate::storage::StorageRoot;
use luma_render::{coords, DEFAULT_SUBFRAMES};
use luma_scene::View;

/// Sub-samples integrated into each output frame.
///
/// A point-sampled frame beats against anything faster than the frame rate —
/// strobes above ~10 Hz and sixteenth-note chases both alias into an irregular
/// stutter. Four samples is what a shutter costs here for free: the renderer's
/// jitter budget ([`DEFAULT_SUBFRAMES`]) is *divided* between them
/// ([`SUBFRAMES`]), so the haze march — which is the whole GPU cost — does the
/// same total work, now spread over time as well as over the pixel.
///
/// Not a flag. A recording that samples time differently from another
/// recording is two answers to one question.
const SHUTTER_SAMPLES: u32 = 4;

/// Fraction of the frame interval the shutter is open, as a cinema shutter
/// angle: 0.5 is the 180° convention.
///
/// It must be less than 1. A fully open shutter integrates a 50%-duty strobe to
/// a constant half-brightness — mathematically the honest answer, and visually
/// the death of the strobe. Half-open keeps the flicker a camera in the room
/// would have recorded.
const SHUTTER_ANGLE: f32 = 0.5;

/// Jitter samples per sub-render. See [`SHUTTER_SAMPLES`].
const SUBFRAMES: u32 = DEFAULT_SUBFRAMES / SHUTTER_SAMPLES;

/// Bits per pixel per second for the hardware encoder's target bitrate.
/// Volumetric haze is dense noise, and a light show is mostly dark with small
/// very bright regions, so this sits above the usual 0.1 rule of thumb.
const BITS_PER_PIXEL: f64 = 0.15;

/// What to record. Everything else is derived from the library.
#[derive(Clone, Debug)]
pub struct Recording {
    /// The score to render. Its track and venue come with it.
    pub score_id: String,
    pub view: View,
    /// Seconds, clamped into the track. `None` records the whole track.
    pub span: Option<(f32, f32)>,
    pub size: (u32, u32),
    pub fps: u32,
    pub output: PathBuf,
}

/// Where a recording has got to. Emitted once per output frame.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub frame: u64,
    pub total: u64,
    /// Wall time since the frame loop started — scene assembly excluded.
    pub elapsed: Duration,
    /// Cumulative GPU-and-assemble time, over every sub-sample.
    pub render: Duration,
    /// Cumulative time spent handing frames to ffmpeg.
    pub encode: Duration,
}

/// What was written.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub path: PathBuf,
    pub frames: u64,
    /// Seconds of video, `frames / fps`.
    pub duration: f32,
    pub elapsed: Duration,
    pub render: Duration,
    pub encode: Duration,
}

/// Set to stop a recording at the next frame boundary.
pub type CancelFlag = Arc<AtomicBool>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecordError {
    #[error("no score {0} in this library")]
    NoScore(String),
    #[error("score {0} has no clips, so there is nothing to light the room with")]
    EmptyScore(String),
    #[error(
        "this venue has no patched fixtures and no stage pieces, so there is nothing to render"
    )]
    EmptyVenue,
    #[error("the track has no known duration, so a recording has no length")]
    NoDuration,
    #[error("--span {0}:{1} is empty or outside the track")]
    EmptySpan(f32, f32),
    #[error("the track's audio file is missing: {0}")]
    NoAudio(PathBuf),
    #[error("ffmpeg could not be started ({0}); is the bundled runtime present?")]
    FfmpegSpawn(#[source] std::io::Error),
    #[error("the frame could not be handed to ffmpeg: {0}")]
    Pipe(#[source] std::io::Error),
    #[error("ffmpeg exited with {status}:\n{stderr}")]
    Ffmpeg { status: String, stderr: String },
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Library(String),
}

impl From<String> for RecordError {
    fn from(message: String) -> Self {
        Self::Library(message)
    }
}

/// Render `spec` to an mp4 with the track's audio.
///
/// Blocks on a blocking-pool thread for the whole render; `cancel` is polled
/// once per frame and `progress` is called once per frame.
///
/// # Errors
/// A missing score or venue, an empty span, a missing audio file, no GPU, or a
/// non-zero ffmpeg exit — whose stderr is in the message.
pub async fn record(
    pool: &SqlitePool,
    storage: &StorageRoot,
    fixtures_root: &Path,
    spec: Recording,
    cancel: CancelFlag,
    progress: impl Fn(Progress) + Send + 'static,
) -> Result<Recorded, RecordError> {
    let session = Session::prepare(pool, storage, fixtures_root, spec).await?;
    tokio::task::spawn_blocking(move || session.run(&cancel, &progress))
        .await
        .map_err(|error| RecordError::Library(format!("the recording task failed: {error}")))?
}

// ---------------------------------------------------------------------------
// the time axis
// ---------------------------------------------------------------------------

/// The grid of moments one recording samples.
///
/// Constructed clamped: a span outside the track, reversed, or non-finite is
/// resolved here rather than being carried as an invariant the frame loop has
/// to keep. The only span that cannot be repaired — one that clamps to nothing
/// — is the single error.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TimeAxis {
    start: f32,
    fps: u32,
    frames: u64,
}

impl TimeAxis {
    /// `fps` is clamped to at least 1: zero frames per second is not a
    /// recording, and refusing it would only push the check to every caller.
    fn new(span: Option<(f32, f32)>, duration: f32, fps: u32) -> Result<Self, RecordError> {
        let fps = fps.max(1);
        let (start, end) = span.unwrap_or((0.0, duration));
        // Checked before the clamp, not after: `f32::clamp` *panics* on a NaN
        // bound, and a track whose duration never got written is exactly that.
        if !(start.is_finite() && end.is_finite() && duration.is_finite()) {
            return Err(RecordError::EmptySpan(start, end));
        }
        let start = start.clamp(0.0, duration.max(0.0));
        let end = end.clamp(0.0, duration.max(0.0));
        // `round`, not `floor`: a span of exactly 10 s at 30 fps is 300 frames,
        // and float division must not turn that into 299.
        let frames = (f64::from(end - start) * f64::from(fps)).round() as i64;
        if frames < 1 {
            return Err(RecordError::EmptySpan(start, end));
        }
        Ok(Self {
            start,
            fps,
            frames: frames as u64,
        })
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn seconds(&self) -> f32 {
        self.frames as f32 / self.fps as f32
    }

    /// When frame `n` opens.
    fn opens(&self, n: u64) -> f32 {
        self.start + n as f32 / self.fps as f32
    }

    /// The moments integrated into frame `n`, stratified across the open part
    /// of its interval.
    fn exposure(&self, n: u64) -> impl Iterator<Item = f32> + use<> {
        let opens = self.opens(n);
        let step = SHUTTER_ANGLE / (self.fps * SHUTTER_SAMPLES) as f32;
        (0..SHUTTER_SAMPLES).map(move |k| opens + (k as f32 + 0.5) * step)
    }
}

// ---------------------------------------------------------------------------
// the encoder
// ---------------------------------------------------------------------------

/// Everything the ffmpeg argv is a function of.
struct Encode<'a> {
    size: (u32, u32),
    fps: u32,
    /// The track file, where in it the video starts, and how long it runs.
    audio: (&'a Path, f32, f32),
    output: &'a Path,
}

/// The video encoder to ask for, and its rate control.
///
/// `h264_videotoolbox` is hardware and effectively free; it exists only on
/// Apple platforms, and it is rate-controlled rather than quality-controlled,
/// hence the two shapes.
fn video_codec(size: (u32, u32), fps: u32) -> Vec<String> {
    if cfg!(target_os = "macos") {
        let rate = f64::from(size.0) * f64::from(size.1) * f64::from(fps) * BITS_PER_PIXEL;
        vec![
            "-c:v".into(),
            "h264_videotoolbox".into(),
            "-b:v".into(),
            format!("{}", rate.round() as u64),
        ]
    } else {
        vec![
            "-c:v".into(),
            "libx264".into(),
            "-crf".into(),
            "18".into(),
            "-preset".into(),
            "veryfast".into(),
        ]
    }
}

/// The full ffmpeg command line, output last.
///
/// Pure, so the shape of the pipe is a unit test rather than a thing you learn
/// by watching a render fail four minutes in.
fn ffmpeg_argv(encode: &Encode) -> Vec<String> {
    let (width, height) = encode.size;
    let mut argv: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        // video: raw frames on stdin, exactly as the renderer hands them back.
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "-s".into(),
        format!("{width}x{height}"),
        "-r".into(),
        encode.fps.to_string(),
        "-i".into(),
        "pipe:0".into(),
    ];
    // `t` is file-relative — it is the audio file's own position — so only a
    // span that does not start at zero needs the input seeked.
    //
    // The audio is cut to the video's own length here rather than with
    // `-shortest`. `-shortest` finalises the file the moment the video pipe
    // reaches EOF, and a pipe that delivers one frame every 50 ms leaves the
    // audio decoder some seven seconds behind when that happens — measured, on
    // every recording, as exactly that much silence missing from the end.
    let (audio, start, seconds) = encode.audio;
    if start > 0.0 {
        argv.push("-ss".into());
        argv.push(format!("{start}"));
    }
    argv.push("-t".into());
    argv.push(format!("{seconds}"));
    argv.push("-i".into());
    argv.push(audio.to_string_lossy().into_owned());
    argv.extend(["-map", "0:v:0", "-map", "1:a:0"].map(String::from));
    argv.extend(video_codec(encode.size, encode.fps));
    argv.extend(
        [
            // yuv420p so every player takes it; bt709 tagged so the beam cores
            // the tonemapper produces are read in the space they were made in.
            "-pix_fmt",
            "yuv420p",
            "-colorspace",
            "bt709",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
        ]
        .map(String::from),
    );
    argv.push(encode.output.to_string_lossy().into_owned());
    argv
}

// ---------------------------------------------------------------------------
// the session
// ---------------------------------------------------------------------------

/// A prepared recording: everything read, compiled and fitted, nothing rendered.
///
/// Split from the frame loop because the loop is blocking and the preparation
/// is `async` — this is the value that crosses onto the blocking pool.
struct Session {
    axis: TimeAxis,
    lighting: crate::eval::Scene,
    geometry: VenueGeometry,
    meshes_root: PathBuf,
    view: View,
    size: (u32, u32),
    audio: PathBuf,
    output: PathBuf,
}

impl Session {
    async fn prepare(
        pool: &SqlitePool,
        storage: &StorageRoot,
        fixtures_root: &Path,
        spec: Recording,
    ) -> Result<Self, RecordError> {
        let (track_id, venue_id) = score_scope(pool, &spec.score_id).await?;
        let clips = clips_of(pool, &venue_id, &spec.score_id).await?;
        if clips.is_empty() {
            return Err(RecordError::EmptyScore(spec.score_id));
        }

        let duration = crate::database::local::tracks::get_track_duration(pool, &track_id)
            .await?
            .ok_or(RecordError::NoDuration)? as f32;
        let axis = TimeAxis::new(spec.span, duration, spec.fps)?;

        let audio = PathBuf::from(
            crate::database::local::tracks::get_track_path_and_hash(pool, &track_id)
                .await?
                .file_path,
        );
        if !audio.is_file() {
            return Err(RecordError::NoAudio(audio));
        }

        let lighting = crate::compositor::build_scene_strict(
            pool,
            pool,
            storage,
            fixtures_root,
            &track_id,
            &venue_id,
            &clips,
        )
        .await?;

        let geometry = VenueGeometry::load(pool, fixtures_root, &venue_id).await?;
        if geometry.is_empty() {
            return Err(RecordError::EmptyVenue);
        }

        Ok(Self {
            axis,
            lighting,
            geometry,
            meshes_root: stage_render::meshes_root(Some(fixtures_root)),
            view: spec.view,
            size: spec.size,
            audio,
            output: spec.output,
        })
    }

    /// The frame loop. Blocks for the length of the render.
    fn run(
        self,
        cancel: &AtomicBool,
        progress: &(dyn Fn(Progress) + Send),
    ) -> Result<Recorded, RecordError> {
        let (scene, definitions) = self.geometry.scene();
        let booth = self.geometry.booth();
        let sequence = Sequence::install(
            scene,
            definitions,
            self.meshes_root.clone(),
            self.view,
            booth,
            self.size,
        )?;
        let (width, height) = sequence.size();

        let argv = ffmpeg_argv(&Encode {
            size: (width, height),
            fps: self.axis.fps,
            audio: (&self.audio, self.axis.start, self.axis.seconds()),
            output: &self.output,
        });
        let mut ffmpeg = Command::new(crate::ffmpeg_env::ffmpeg_path())
            .args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(RecordError::FfmpegSpawn)?;
        let mut stdin = ffmpeg.stdin.take().expect("stdin was piped");
        // Drained on its own thread: a full stderr pipe would deadlock the
        // frame loop against an encoder that is trying to explain itself.
        let stderr = ffmpeg.stderr.take().expect("stderr was piped");
        let watcher = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::BufReader::new(stderr), &mut text);
            text
        });

        let mut arena = Arena::default();
        let mut exposure = Exposure::new(width, height);
        let started = Instant::now();
        let (mut render, mut encode) = (Duration::ZERO, Duration::ZERO);
        let mut written = 0u64;
        let mut broken = None;

        for n in 0..self.axis.frames() {
            if cancel.load(Ordering::Relaxed) {
                broken = Some(RecordError::Cancelled);
                break;
            }
            let clock = Instant::now();
            let times: Vec<f32> = self.axis.exposure(n).collect();
            let states: Vec<UniverseState> =
                self.lighting.render(&times, Scope::Composite, &mut arena);
            exposure.reset();
            for (k, &t) in times.iter().enumerate() {
                exposure.add(&sequence.frame(states.get(k), t, SUBFRAMES)?);
            }
            render += clock.elapsed();

            let clock = Instant::now();
            if let Err(error) = stdin.write_all(exposure.resolve()) {
                // A broken pipe means ffmpeg died; its stderr is the real
                // diagnosis, so fall through to the exit check.
                broken = Some(RecordError::Pipe(error));
                break;
            }
            encode += clock.elapsed();
            written += 1;
            progress(Progress {
                frame: written,
                total: self.axis.frames(),
                elapsed: started.elapsed(),
                render,
                encode,
            });
        }
        let elapsed = started.elapsed();

        drop(stdin);
        let status = ffmpeg.wait().map_err(RecordError::Pipe)?;
        let stderr = watcher.join().unwrap_or_default();
        if let Some(error) = broken {
            if !status.success() {
                return Err(RecordError::Ffmpeg {
                    status: status.to_string(),
                    stderr,
                });
            }
            return Err(error);
        }
        if !status.success() {
            return Err(RecordError::Ffmpeg {
                status: status.to_string(),
                stderr,
            });
        }
        Ok(Recorded {
            path: self.output,
            frames: written,
            duration: self.axis.seconds(),
            elapsed,
            render,
            encode,
        })
    }
}

/// Accumulates the exposure of one output frame in linear light.
///
/// The renderer hands back sRGB-encoded bytes, so averaging them as bytes
/// averages a gamma curve — a half-duty strobe would come out at 50% of the
/// *code value* instead of 50% of the light, about a stop and a half too dark.
/// Decode, sum, re-encode.
struct Exposure {
    linear: Vec<f32>,
    out: Vec<u8>,
    samples: f32,
}

impl Exposure {
    /// sRGB decode of every possible byte, because the alternative is a `powf`
    /// per channel per sub-sample — 11 million of them per 720p frame.
    fn table() -> [f32; 256] {
        std::array::from_fn(|byte| coords::srgb_to_linear(byte as f32 / 255.0))
    }

    fn new(width: u32, height: u32) -> Self {
        let pixels = width as usize * height as usize;
        Self {
            linear: vec![0.0; pixels * 3],
            out: vec![0xff; pixels * 4],
            samples: 0.0,
        }
    }

    fn reset(&mut self) {
        self.linear.fill(0.0);
        self.samples = 0.0;
    }

    fn add(&mut self, rgba: &[u8]) {
        static TABLE: std::sync::LazyLock<[f32; 256]> = std::sync::LazyLock::new(Exposure::table);
        for (acc, pixel) in self.linear.chunks_exact_mut(3).zip(rgba.chunks_exact(4)) {
            acc[0] += TABLE[pixel[0] as usize];
            acc[1] += TABLE[pixel[1] as usize];
            acc[2] += TABLE[pixel[2] as usize];
        }
        self.samples += 1.0;
    }

    /// The mean exposure, re-encoded. Alpha is left opaque: the pipe is
    /// `yuv420p` and nothing downstream reads it.
    fn resolve(&mut self) -> &[u8] {
        let scale = if self.samples > 0.0 {
            1.0 / self.samples
        } else {
            0.0
        };
        for (pixel, acc) in self
            .out
            .chunks_exact_mut(4)
            .zip(self.linear.chunks_exact(3))
        {
            for c in 0..3 {
                pixel[c] =
                    (coords::linear_to_srgb(acc[c] * scale).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
        &self.out
    }
}

/// The `(track, venue)` a score belongs to.
///
/// A plain lookup by id: every read that follows goes through `VenueAccess`,
/// which is where the venue gate actually is.
async fn score_scope(pool: &SqlitePool, score_id: &str) -> Result<(String, String), RecordError> {
    sqlx::query_as::<_, (String, String)>("SELECT track_id, venue_id FROM scores WHERE id = ?")
        .bind(score_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| RecordError::Library(format!("could not read the score: {error}")))?
        .ok_or_else(|| RecordError::NoScore(score_id.to_string()))
}

/// One score's own clips.
///
/// **Smell.** The live compositor's `scores_for_track` takes every clip of
/// every score on a `(track, venue)` — score identity is not part of what it
/// composites. A recording of "score X" here is X's clips alone, which is what
/// the request means and what the venue shows whenever a track has one score.
/// Where a track has two, the two paths disagree, and the compositor is the
/// half that should grow a score id.
async fn clips_of(
    pool: &SqlitePool,
    venue_id: &str,
    score_id: &str,
) -> Result<Vec<TrackScore>, RecordError> {
    use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id))
        .await
        .map_err(|error| RecordError::Library(format!("the venue is not available: {error}")))?;
    Ok(crate::database::local::scores::get_clips_of_score(&mut access, score_id).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_track_is_the_default_span() {
        let axis = TimeAxis::new(None, 10.0, 30).expect("a ten second track records");
        assert_eq!(axis.frames(), 300);
        assert!((axis.opens(0) - 0.0).abs() < 1e-6);
        // The last frame opens one interval before the end, not at it.
        assert!((axis.opens(299) - (10.0 - 1.0 / 30.0)).abs() < 1e-5);
        assert!((axis.seconds() - 10.0).abs() < 1e-6);
    }

    #[test]
    fn a_span_is_clamped_into_the_track_rather_than_refused() {
        let axis = TimeAxis::new(Some((-5.0, 400.0)), 10.0, 30).expect("clamps to the track");
        assert_eq!(axis, TimeAxis::new(None, 10.0, 30).unwrap());
    }

    #[test]
    fn a_span_records_only_its_own_frames() {
        let axis = TimeAxis::new(Some((30.0, 40.0)), 200.0, 25).expect("a ten second span");
        assert_eq!(axis.frames(), 250);
        assert!((axis.opens(0) - 30.0).abs() < 1e-5);
    }

    #[test]
    fn a_span_that_clamps_to_nothing_is_the_one_error() {
        assert!(matches!(
            TimeAxis::new(Some((40.0, 30.0)), 200.0, 30),
            Err(RecordError::EmptySpan(..))
        ));
        assert!(matches!(
            TimeAxis::new(Some((300.0, 400.0)), 200.0, 30),
            Err(RecordError::EmptySpan(..))
        ));
        assert!(matches!(
            TimeAxis::new(None, f32::NAN, 30),
            Err(RecordError::EmptySpan(..))
        ));
    }

    #[test]
    fn zero_fps_is_repaired_not_refused() {
        let axis = TimeAxis::new(None, 4.0, 0).expect("clamped to one frame per second");
        assert_eq!(axis.frames(), 4);
    }

    #[test]
    fn the_exposure_stays_inside_the_open_part_of_the_frame() {
        let axis = TimeAxis::new(None, 10.0, 30).unwrap();
        let interval = 1.0 / 30.0;
        for n in [0u64, 1, 299] {
            let times: Vec<f32> = axis.exposure(n).collect();
            assert_eq!(times.len(), SHUTTER_SAMPLES as usize);
            let opens = axis.opens(n);
            assert!(times[0] > opens, "the first sample is inside the interval");
            assert!(
                *times.last().unwrap() < opens + interval * SHUTTER_ANGLE,
                "the last sample is inside the open part"
            );
            // Stratified, so the samples are evenly spread and in order.
            for pair in times.windows(2) {
                assert!(pair[1] > pair[0]);
            }
        }
    }

    #[test]
    fn the_argv_pipes_raw_video_in_and_muxes_the_track_audio() {
        let argv = ffmpeg_argv(&Encode {
            size: (1280, 720),
            fps: 30,
            audio: (Path::new("/tracks/a.mp3"), 0.0, 10.0),
            output: Path::new("/out/a.mp4"),
        });
        let line = argv.join(" ");
        assert!(line.contains("-f rawvideo -pix_fmt rgba -s 1280x720 -r 30 -i pipe:0"));
        assert!(line.contains("-t 10 -i /tracks/a.mp3 -map 0:v:0 -map 1:a:0"));
        assert!(line.contains("-pix_fmt yuv420p"));
        assert!(line.contains("-c:a aac"));
        assert!(line.contains("-movflags +faststart"));
        assert_eq!(argv.last().unwrap(), "/out/a.mp4");
        // No seek when the recording starts at the top of the track.
        assert!(!line.contains("-ss"));
    }

    #[test]
    fn a_span_seeks_the_audio_input_only() {
        let argv = ffmpeg_argv(&Encode {
            size: (640, 360),
            fps: 25,
            audio: (Path::new("/tracks/a.mp3"), 30.0, 12.5),
            output: Path::new("/out/a.mp4"),
        });
        let seek = argv.iter().position(|a| a == "-ss").expect("seeks");
        let audio = argv
            .iter()
            .rposition(|a| a == "-i")
            .expect("has an audio input");
        assert_eq!(argv[seek + 1], "30");
        assert!(seek < audio, "the seek applies to the audio input");
    }

    #[test]
    fn the_encoder_matches_the_platform() {
        let argv = ffmpeg_argv(&Encode {
            size: (1920, 1080),
            fps: 30,
            audio: (Path::new("/a.mp3"), 0.0, 3.0),
            output: Path::new("/o.mp4"),
        });
        if cfg!(target_os = "macos") {
            assert!(argv.contains(&"h264_videotoolbox".to_string()));
            // 1920x1080x30 at 0.15 bpp.
            assert!(argv.contains(&"9331200".to_string()));
        } else {
            assert!(argv.contains(&"libx264".to_string()));
        }
    }

    #[test]
    fn averaging_happens_in_linear_light() {
        // Half the sub-frames fully lit, half black: a 50%-duty strobe. Linear
        // mean is 0.5, which is code value ~188, not 128.
        let mut exposure = Exposure::new(1, 1);
        exposure.add(&[255, 255, 255, 255]);
        exposure.add(&[0, 0, 0, 255]);
        let out = exposure.resolve();
        assert!(
            (186..=190).contains(&out[0]),
            "half exposure came out at {}",
            out[0]
        );
        assert_eq!(out[3], 255);
    }

    #[test]
    fn a_constant_exposure_survives_the_round_trip() {
        let mut exposure = Exposure::new(2, 1);
        for _ in 0..SHUTTER_SAMPLES {
            exposure.add(&[17, 128, 240, 255, 3, 99, 200, 255]);
        }
        let out = exposure.resolve();
        for (got, want) in out.iter().zip([17u8, 128, 240, 255, 3, 99, 200, 255]) {
            assert!((i16::from(*got) - i16::from(want)).abs() <= 1);
        }
    }
}
