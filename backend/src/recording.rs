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
use crate::models::universe::UniverseState;
use crate::stage_render::{self, Continuity, Sequence, VenueGeometry};
use crate::storage::StorageRoot;
use luma_render::frame::Moment;
use luma_render::scene_desc::Footage;
use luma_render::{footage, DEFAULT_SUBFRAMES, LIVE_SUBFRAMES};
use luma_scene::View;

/// How a recording integrates time into each output frame.
///
/// The renderer's haze march is stochastic, and there are two ways to pay for
/// a clean one. Both are here because they are not interchangeable — one is a
/// camera and one is a memory — and which one a recording wants is a question
/// about the *content*, not a quality dial.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Haze {
    /// Every output frame is a function of its own `t` alone.
    ///
    /// [`DEFAULT_SUBFRAMES`] haze marches per output frame, with the
    /// renderer's temporal history cut before each frame so nothing leaks
    /// between them. Deterministic and slow.
    #[default]
    Accumulate,
    /// Frames are consecutive, so let the renderer remember.
    ///
    /// Exactly the live viewport's path: [`LIVE_SUBFRAMES`] marches per frame,
    /// blended into the haze history at 18% each, warmed up over
    /// `WARMUP_FRAMES` discarded frames before the span starts. Eight times
    /// fewer marches than [`Haze::Accumulate`].
    ///
    /// The tail is also conditional. The renderer resets its history whenever
    /// the *cone geometry* changes — any moving head, every frame — so on a
    /// rig with movers this mode degrades to a bare [`LIVE_SUBFRAMES`]-sample
    /// march with no integration at all. See §9.
    Temporal,
}

impl Haze {
    /// Jitter samples per output frame. The renderer shares them out between
    /// a frame's shutter moments.
    const fn subframes(self) -> u32 {
        match self {
            Self::Accumulate => DEFAULT_SUBFRAMES,
            Self::Temporal => LIVE_SUBFRAMES,
        }
    }

    const fn continuity(self) -> Continuity {
        match self {
            Self::Accumulate => Continuity::Cut,
            Self::Temporal => Continuity::Next,
        }
    }

    /// Frames rendered and thrown away before the span, so the history is
    /// converged when the first kept frame is drawn.
    const fn warmup(self) -> u64 {
        match self {
            Self::Accumulate => 0,
            Self::Temporal => WARMUP_FRAMES,
        }
    }
}

impl std::str::FromStr for Haze {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "accumulate" => Ok(Self::Accumulate),
            "temporal" => Ok(Self::Temporal),
            other => Err(format!(
                "--haze takes accumulate or temporal, not {other:?}"
            )),
        }
    }
}

/// Frames [`Haze::Temporal`] draws and discards before the span starts.
///
/// The temporal resolve keeps 82% of the history and mixes in 18% of the new
/// march, so the weight of the unconverged first frame decays as `0.82^n`:
/// under 5% by 16 frames and under 1% by 24. Twenty-four is 0.8 s at 30 fps
/// and about a second of wall time — cheap insurance against a recording that
/// opens visibly noisier than it ends.
const WARMUP_FRAMES: u64 = 24;

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
    /// How each output frame integrates time.
    pub haze: Haze,
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
    haze: Haze,
}

impl TimeAxis {
    /// `fps` is clamped to at least 1: zero frames per second is not a
    /// recording, and refusing it would only push the check to every caller.
    fn new(
        span: Option<(f32, f32)>,
        duration: f32,
        fps: u32,
        haze: Haze,
    ) -> Result<Self, RecordError> {
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
            haze,
        })
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn seconds(&self) -> f32 {
        self.frames as f32 / self.fps as f32
    }

    /// Frame `n`'s time on the track. Its shutter closes here, as a show
    /// export's frame closes on its tick.
    fn time(&self, n: u64) -> f32 {
        self.start + n as f32 / self.fps as f32
    }

    /// The moments of the frame whose shutter closes at `time`: what live and
    /// the show export draw, through [`footage::moments`]. The recording's
    /// clock is the track's, so a strobe flashes in step with the music.
    ///
    /// With the footage look off, one moment whose strobes are the light of
    /// the whole frame interval. With it on, [`footage::EXPORT_SUBFRAMES`]
    /// whole moments across the open part of the interval. A recording has no
    /// bass envelope, so the bass shake stays still.
    fn moments(&self, time: f32, look: &Footage) -> Vec<Moment> {
        footage::moments(
            look,
            f64::from(time),
            1.0 / f64::from(self.fps),
            footage::EXPORT_SUBFRAMES,
            |_| 0.0,
        )
    }

    /// The moments drawn and discarded before frame zero, in order, so that the
    /// renderer's haze history is converged by the time the span opens.
    ///
    /// They run up to the start on the output grid, so the last of them is one
    /// frame interval before the first kept frame and the history stays
    /// continuous across the join. Clamped at zero: a recording that starts at
    /// the top of the track warms up on its own first moment, which converges
    /// the haze without inventing a `t` the score has no state for.
    fn warmup(&self) -> impl Iterator<Item = f32> + use<> {
        let (start, fps) = (self.start, self.fps);
        let count = self.haze.warmup();
        (0..count).map(move |i| (start - (count - i) as f32 / fps as f32).max(0.0))
    }
}

// ---------------------------------------------------------------------------
// the encoder
// ---------------------------------------------------------------------------

/// Everything the ffmpeg argv is a function of.
pub struct Encode<'a> {
    pub size: (u32, u32),
    pub fps: u32,
    /// The track file, where in it the video starts, and how long it runs.
    /// `None` writes a silent file.
    pub audio: Option<(&'a Path, f32, f32)>,
    pub output: &'a Path,
}

/// The video encoder to ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    /// Apple's hardware H.264. Rate-controlled rather than quality-controlled.
    VideoToolbox,
    /// NVIDIA's hardware HEVC. Much faster than x264 at 4K.
    Nvenc,
    /// Software H.264, which every ffmpeg build has.
    X264,
}

impl Codec {
    fn args(self, size: (u32, u32), fps: u32) -> Vec<String> {
        let args: &[&str] = match self {
            Self::VideoToolbox => {
                let rate = f64::from(size.0) * f64::from(size.1) * f64::from(fps) * BITS_PER_PIXEL;
                return vec![
                    "-c:v".into(),
                    "h264_videotoolbox".into(),
                    "-b:v".into(),
                    format!("{}", rate.round() as u64),
                ];
            }
            // Main10 with spatial and temporal AQ: a show is mostly dark haze
            // gradients, which 8-bit bands and plain CQ starves of bits.
            // `hvc1` so Apple players open the file too.
            Self::Nvenc => &[
                "-c:v",
                "hevc_nvenc",
                "-preset",
                "p5",
                "-tune",
                "hq",
                "-rc",
                "vbr",
                "-cq",
                "16",
                "-b:v",
                "0",
                "-spatial-aq",
                "1",
                "-temporal-aq",
                "1",
                "-rc-lookahead",
                "32",
                "-profile:v",
                "main10",
                "-tag:v",
                "hvc1",
            ],
            Self::X264 => &["-c:v", "libx264", "-preset", "fast", "-crf", "16"],
        };
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    fn pixel_format(self) -> &'static str {
        match self {
            Self::Nvenc => "p010le",
            Self::VideoToolbox | Self::X264 => "yuv420p",
        }
    }
}

/// The ffmpeg to run and the encoder to ask it for.
///
/// On Linux and Windows, NVENC wins where one of the two ffmpegs — the bundled
/// one, then the one on `PATH` — can open it. Listing it in `-encoders` is not
/// enough: distribution builds list it on machines with no NVIDIA GPU, so one
/// black frame is encoded to find out. The bundled static build has no NVENC.
fn pick_encoder() -> (PathBuf, Codec) {
    let bundled = crate::ffmpeg_env::ffmpeg_path();
    if cfg!(target_os = "macos") {
        return (bundled, Codec::VideoToolbox);
    }
    let nvenc = |ffmpeg: &Path| {
        Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=black:s=256x256",
                "-frames:v",
                "1",
                "-c:v",
                "hevc_nvenc",
                "-f",
                "null",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    [bundled.clone(), PathBuf::from("ffmpeg")]
        .into_iter()
        .find(|ffmpeg| nvenc(ffmpeg))
        .map_or((bundled, Codec::X264), |ffmpeg| (ffmpeg, Codec::Nvenc))
}

/// The full ffmpeg command line, output last.
///
/// Pure, so the shape of the pipe is a unit test rather than a thing you learn
/// by watching a render fail four minutes in.
fn ffmpeg_argv(encode: &Encode, codec: Codec) -> Vec<String> {
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
    if let Some((audio, start, seconds)) = encode.audio {
        if start > 0.0 {
            argv.push("-ss".into());
            argv.push(format!("{start}"));
        }
        argv.push("-t".into());
        argv.push(format!("{seconds}"));
        argv.push("-i".into());
        argv.push(audio.to_string_lossy().into_owned());
        argv.extend(["-map", "0:v:0", "-map", "1:a:0"].map(String::from));
        argv.extend(["-c:a", "aac", "-b:a", "192k"].map(String::from));
    }
    argv.extend(codec.args(encode.size, encode.fps));
    // The frames are sRGB. Converted to YUV with the BT.709 matrix and tagged
    // BT.709, so players decode the colours the stage drew: without the
    // explicit matrix swscale converts with BT.601 and the tag names a matrix
    // the pixels were not made with.
    argv.push("-vf".into());
    argv.push(format!(
        "scale=out_color_matrix=bt709:out_range=tv,format={}",
        codec.pixel_format()
    ));
    argv.extend(
        [
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-color_range",
            "tv",
            "-movflags",
            "+faststart",
        ]
        .map(String::from),
    );
    argv.push(encode.output.to_string_lossy().into_owned());
    argv
}

/// Frames queued between the caller and ffmpeg. A slow encoder holds the
/// caller back here rather than buffering the film in memory.
const ENCODE_QUEUE: usize = 2;

/// A running ffmpeg writing raw RGBA frames to an mp4.
///
/// Frames cross to ffmpeg on a thread of their own, so the caller renders the
/// next frame while the last one is in the pipe. Dropped before
/// [`Encoder::finish`], it kills ffmpeg and deletes the partial file: a file
/// at `output` is a finished one.
pub struct Encoder {
    child: std::process::Child,
    frames: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    writer: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    stderr: Option<std::thread::JoinHandle<String>>,
    output: PathBuf,
    closed: bool,
}

impl Encoder {
    /// Start ffmpeg on the best encoder this machine has.
    ///
    /// # Errors
    /// ffmpeg could not be started.
    pub fn start(encode: &Encode) -> Result<Self, RecordError> {
        let (ffmpeg, codec) = pick_encoder();
        let mut child = Command::new(ffmpeg)
            .args(ffmpeg_argv(encode, codec))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(RecordError::FfmpegSpawn)?;
        let mut stdin = child.stdin.take().expect("stdin was piped");
        // Drained on its own thread: a full stderr pipe would deadlock the
        // frame loop against an encoder that is trying to explain itself.
        let stderr = child.stderr.take().expect("stderr was piped");
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::BufReader::new(stderr), &mut text);
            text
        });
        let (frames, queued) = std::sync::mpsc::sync_channel::<Vec<u8>>(ENCODE_QUEUE);
        // Ends when the sender is dropped, closing stdin with it: that EOF
        // is what tells ffmpeg the film is over.
        let writer = std::thread::spawn(move || {
            for frame in queued {
                stdin.write_all(&frame)?;
            }
            Ok(())
        });
        Ok(Self {
            child,
            frames: Some(frames),
            writer: Some(writer),
            stderr: Some(stderr),
            output: encode.output.to_path_buf(),
            closed: false,
        })
    }

    /// Queue one frame. Waits while ffmpeg is [`ENCODE_QUEUE`] frames behind.
    ///
    /// # Errors
    /// ffmpeg stopped taking frames; its own explanation when it gave one.
    pub fn write(&mut self, frame: Vec<u8>) -> Result<(), RecordError> {
        if self
            .frames
            .as_ref()
            .is_some_and(|frames| frames.send(frame).is_ok())
        {
            return Ok(());
        }
        Err(self.close().err().unwrap_or_else(|| {
            RecordError::Pipe(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }))
    }

    /// End the stream and wait for ffmpeg to finish the file.
    ///
    /// # Errors
    /// A frame could not be handed over, or ffmpeg exited non-zero — its
    /// stderr is in the message. The partial file is deleted.
    pub fn finish(mut self) -> Result<(), RecordError> {
        self.close()
    }

    fn close(&mut self) -> Result<(), RecordError> {
        self.closed = true;
        drop(self.frames.take());
        let written = self
            .writer
            .take()
            .map_or(Ok(()), |writer| writer.join().unwrap_or(Ok(())));
        let status = self.child.wait().map_err(RecordError::Pipe);
        let stderr = self
            .stderr
            .take()
            .map(|stderr| stderr.join().unwrap_or_default())
            .unwrap_or_default();
        let result = match status {
            Ok(status) if !status.success() => Err(RecordError::Ffmpeg {
                status: status.to_string(),
                stderr,
            }),
            Ok(_) => written.map_err(RecordError::Pipe),
            Err(error) => Err(error),
        };
        if result.is_err() {
            let _ = std::fs::remove_file(&self.output);
        }
        result
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let _ = self.child.kill();
        let _ = self.close();
        let _ = std::fs::remove_file(&self.output);
    }
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

        let duration = crate::database::local::tracks::get_track_duration(pool, &track_id)
            .await?
            .ok_or(RecordError::NoDuration)? as f32;
        let axis = TimeAxis::new(spec.span, duration, spec.fps, spec.haze)?;

        let audio = PathBuf::from(
            crate::database::local::tracks::get_track_path_and_hash(pool, &track_id)
                .await?
                .file_path,
        );
        if !audio.is_file() {
            return Err(RecordError::NoAudio(audio));
        }

        let lighting = crate::compositor::build_score_scene(
            pool,
            storage,
            fixtures_root,
            &spec.score_id,
            None,
        )
        .await?;
        if lighting.annotations.is_empty() {
            return Err(RecordError::EmptyScore(spec.score_id));
        }

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
        let look = scene.render.look.footage;
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

        let mut encoder = Encoder::start(&Encode {
            size: (width, height),
            fps: self.axis.fps,
            audio: Some((&self.audio, self.axis.start, self.axis.seconds())),
            output: &self.output,
        })?;

        let haze = self.axis.haze;
        let mut arena = Arena::default();
        // The look is the venue scene's own. `VenueGeometry::scene` gives the
        // neutral look, so the footage look is off and each frame is one
        // moment, its strobes integrated over the whole frame interval.
        // Each moment's light state, at its own track time. Before the top of
        // the track the score holds its first state.
        let exposure = |time: f32, arena: &mut Arena| {
            let moments = self.axis.moments(time, &look);
            let times: Vec<f32> = moments
                .iter()
                .map(|moment| moment.time.max(0.0) as f32)
                .collect();
            let states: Vec<UniverseState> = self.lighting.render(&times, Scope::Composite, arena);
            let mut states = states.into_iter();
            moments
                .into_iter()
                .map(|moment| (states.next(), moment))
                .collect::<Vec<_>>()
        };
        // Warm the haze history before the clock starts, so the discarded
        // frames do not land in the measured cost of the kept ones.
        for t in self.axis.warmup() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            sequence.exposure(exposure(t, &mut arena), haze.subframes(), haze.continuity())?;
        }
        let started = Instant::now();
        let (mut render, mut encode) = (Duration::ZERO, Duration::ZERO);
        let mut written = 0u64;
        let mut cancelled = false;

        for n in 0..self.axis.frames() {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let clock = Instant::now();
            let pixels = sequence.exposure(
                exposure(self.axis.time(n), &mut arena),
                haze.subframes(),
                haze.continuity(),
            )?;
            render += clock.elapsed();

            let clock = Instant::now();
            encoder.write(pixels)?;
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

        // A cancelled recording is still closed the ordinary way, so what
        // was rendered is a playable file.
        encoder.finish()?;
        if cancelled {
            return Err(RecordError::Cancelled);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_track_is_the_default_span() {
        let axis =
            TimeAxis::new(None, 10.0, 30, Haze::Accumulate).expect("a ten second track records");
        assert_eq!(axis.frames(), 300);
        assert!((axis.time(0) - 0.0).abs() < 1e-6);
        // The last frame is one interval before the end, not at it.
        assert!((axis.time(299) - (10.0 - 1.0 / 30.0)).abs() < 1e-5);
        assert!((axis.seconds() - 10.0).abs() < 1e-6);
    }

    #[test]
    fn a_span_is_clamped_into_the_track_rather_than_refused() {
        let axis = TimeAxis::new(Some((-5.0, 400.0)), 10.0, 30, Haze::Accumulate)
            .expect("clamps to the track");
        assert_eq!(
            axis,
            TimeAxis::new(None, 10.0, 30, Haze::Accumulate).unwrap()
        );
    }

    #[test]
    fn a_span_records_only_its_own_frames() {
        let axis = TimeAxis::new(Some((30.0, 40.0)), 200.0, 25, Haze::Accumulate)
            .expect("a ten second span");
        assert_eq!(axis.frames(), 250);
        assert!((axis.time(0) - 30.0).abs() < 1e-5);
    }

    #[test]
    fn a_span_that_clamps_to_nothing_is_the_one_error() {
        assert!(matches!(
            TimeAxis::new(Some((40.0, 30.0)), 200.0, 30, Haze::Accumulate),
            Err(RecordError::EmptySpan(..))
        ));
        assert!(matches!(
            TimeAxis::new(Some((300.0, 400.0)), 200.0, 30, Haze::Accumulate),
            Err(RecordError::EmptySpan(..))
        ));
        assert!(matches!(
            TimeAxis::new(None, f32::NAN, 30, Haze::Accumulate),
            Err(RecordError::EmptySpan(..))
        ));
    }

    #[test]
    fn zero_fps_is_repaired_not_refused() {
        let axis =
            TimeAxis::new(None, 4.0, 0, Haze::Accumulate).expect("clamped to one frame per second");
        assert_eq!(axis.frames(), 4);
    }

    #[test]
    fn with_the_look_off_a_frame_is_one_moment_lit_by_its_whole_interval() {
        let axis = TimeAxis::new(None, 10.0, 30, Haze::Accumulate).unwrap();
        for n in [0u64, 1, 299] {
            let moments = axis.moments(axis.time(n), &Footage::OFF);
            assert_eq!(moments.len(), 1);
            let moment = moments[0];
            // The shutter closes on the frame's time, and the strobes see the
            // whole interval since the frame before.
            assert!((moment.time - f64::from(axis.time(n))).abs() < 1e-9);
            assert!((moment.slice.open + moment.slice.length - moment.time).abs() < 1e-9);
            assert!((moment.slice.length - 1.0 / 30.0).abs() < 1e-9);
        }
    }

    #[test]
    fn with_the_look_on_a_frame_is_the_export_shutter() {
        let axis = TimeAxis::new(None, 10.0, 30, Haze::Accumulate).unwrap();
        let look = Footage {
            enabled: true,
            ..Footage::OFF
        };
        let end = f64::from(axis.time(30));
        let moments = axis.moments(axis.time(30), &look);
        assert_eq!(moments.len(), footage::EXPORT_SUBFRAMES as usize);
        // A 180 degree shutter: the last half of the interval, in order.
        assert!((moments[0].slice.open - (end - 0.5 / 30.0)).abs() < 1e-6);
        for pair in moments.windows(2) {
            assert!(pair[1].time > pair[0].time);
        }
        let last = moments.last().unwrap().slice;
        assert!((last.open + last.length - end).abs() < 1e-6);
    }

    #[test]
    fn only_temporal_warms_up_and_it_lands_on_the_output_grid() {
        assert_eq!(
            TimeAxis::new(None, 10.0, 30, Haze::Accumulate)
                .unwrap()
                .warmup()
                .count(),
            0
        );
        let axis = TimeAxis::new(Some((30.0, 40.0)), 200.0, 30, Haze::Temporal).unwrap();
        let warm: Vec<f32> = axis.warmup().collect();
        assert_eq!(warm.len(), WARMUP_FRAMES as usize);
        // Ascending, and the last one is one interval before frame zero, so
        // the history is continuous across the join.
        for pair in warm.windows(2) {
            assert!(pair[1] > pair[0]);
        }
        assert!((warm[warm.len() - 1] - (30.0 - 1.0 / 30.0)).abs() < 1e-4);
        assert!(warm[0] >= 0.0);
    }

    #[test]
    fn a_warm_up_never_walks_off_the_front_of_the_track() {
        let axis = TimeAxis::new(None, 10.0, 30, Haze::Temporal).unwrap();
        assert!(axis.warmup().all(|t| t >= 0.0));
    }

    #[test]
    fn the_two_modes_spend_the_export_and_the_live_haze_budgets() {
        assert_eq!(Haze::Accumulate.subframes(), DEFAULT_SUBFRAMES);
        assert_eq!(Haze::Temporal.subframes(), LIVE_SUBFRAMES);
        assert_eq!(Haze::Accumulate.continuity(), Continuity::Cut);
        assert_eq!(Haze::Temporal.continuity(), Continuity::Next);
    }

    #[test]
    fn a_mode_is_named_by_its_flag_value() {
        use std::str::FromStr;
        assert_eq!(Haze::from_str("temporal"), Ok(Haze::Temporal));
        assert_eq!(Haze::from_str("accumulate"), Ok(Haze::Accumulate));
        assert_eq!(Haze::default(), Haze::Accumulate);
        assert!(Haze::from_str("Temporal").is_err());
        assert!(Haze::from_str("").is_err());
    }

    #[test]
    fn the_argv_pipes_raw_video_in_and_muxes_the_track_audio() {
        let argv = ffmpeg_argv(
            &Encode {
                size: (1280, 720),
                fps: 30,
                audio: Some((Path::new("/tracks/a.mp3"), 0.0, 10.0)),
                output: Path::new("/out/a.mp4"),
            },
            Codec::X264,
        );
        let line = argv.join(" ");
        assert!(line.contains("-f rawvideo -pix_fmt rgba -s 1280x720 -r 30 -i pipe:0"));
        assert!(line.contains("-t 10 -i /tracks/a.mp3 -map 0:v:0 -map 1:a:0"));
        // Converted with the matrix it is tagged with.
        assert!(line.contains("out_color_matrix=bt709"));
        assert!(line.contains("format=yuv420p"));
        assert!(line.contains("-colorspace bt709"));
        assert!(line.contains("-c:a aac"));
        assert!(line.contains("-movflags +faststart"));
        assert_eq!(argv.last().unwrap(), "/out/a.mp4");
        // No seek when the recording starts at the top of the track.
        assert!(!line.contains("-ss"));
    }

    #[test]
    fn a_span_seeks_the_audio_input_only() {
        let argv = ffmpeg_argv(
            &Encode {
                size: (640, 360),
                fps: 25,
                audio: Some((Path::new("/tracks/a.mp3"), 30.0, 12.5)),
                output: Path::new("/out/a.mp4"),
            },
            Codec::X264,
        );
        let seek = argv.iter().position(|a| a == "-ss").expect("seeks");
        let audio = argv
            .iter()
            .rposition(|a| a == "-i")
            .expect("has an audio input");
        assert_eq!(argv[seek + 1], "30");
        assert!(seek < audio, "the seek applies to the audio input");
    }

    #[test]
    fn a_silent_file_has_one_input_and_no_audio_stream() {
        let argv = ffmpeg_argv(
            &Encode {
                size: (640, 360),
                fps: 60,
                audio: None,
                output: Path::new("/out/a.mp4"),
            },
            Codec::Nvenc,
        );
        assert_eq!(argv.iter().filter(|arg| *arg == "-i").count(), 1);
        assert!(!argv.iter().any(|arg| arg == "-map" || arg == "aac"));
        assert!(argv.contains(&"hevc_nvenc".to_string()));
    }

    #[test]
    fn videotoolbox_is_rate_controlled_by_the_frame_size() {
        // 1920x1080x30 at 0.15 bpp.
        assert!(Codec::VideoToolbox
            .args((1920, 1080), 30)
            .contains(&"9331200".to_string()));
    }
}
