//! One score, rendered to an mp4 with the track's own audio.
//!
//! The one way a score becomes a video: the app's "Export show" and the
//! `luma-cli record` CLI both start a [`Recording`] here and wait. This module owns
//! the *time axis* and the *container* — span clamping, frame count, shutter,
//! encoder choice, the audio offset, the ffmpeg argv — and draws each frame
//! the way the stage does: the same score compile, the same motor lag, the
//! same bass under the footage look, and the live pass chain through
//! [`Recorder`], read back at ten bits a channel.
//!
//! Scene assembly happens *once*: the venue, the score and the audio are read
//! before frame zero, and each frame carries only its light state and moment.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::SqlitePool;

use crate::dispatch::AppServices;
use crate::eval::{Arena, Scope};
use crate::host_audio::TrackAudio;
use crate::motors::Motors;
use crate::stage_render::{self, Aim, VenueGeometry};
use luma_render::frame::Moment;
use luma_render::{assets, build_frame_at, footage, Recorder};
use luma_scene::coords;

/// Frames drawn and discarded before the span starts.
///
/// The renderer's temporal resolve keeps 82% of the haze history and mixes in
/// 18% of the new march, so the weight of the unconverged first frame decays
/// as `0.82^n`: under 5% by 16 frames and under 1% by 24. Twenty-four is
/// 0.8 s at 30 fps and about a second of wall time — cheap insurance against a
/// recording that opens visibly noisier than it ends.
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
    /// The editor's working copy of the score, when one is open, so the
    /// recording lights what the stage lights. `None` records the saved score.
    pub score: Option<luma_patterns::Score>,
    pub aim: Aim,
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
    #[error("the track's audio is not on this device: {0}")]
    NoAudio(String),
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
/// The audio is fetched the way playback fetches it when it is not on this
/// device yet. Blocks on a blocking-pool thread for the whole render; `cancel`
/// is polled once per frame and `progress` is called once per frame.
///
/// # Errors
/// A missing score or venue, an empty span, audio that cannot be fetched, no
/// GPU, or a non-zero ffmpeg exit — whose stderr is in the message.
pub async fn record(
    services: &AppServices,
    spec: Recording,
    cancel: CancelFlag,
    progress: impl Fn(Progress) + Send + 'static,
) -> Result<Recorded, RecordError> {
    let session = Session::prepare(services, spec).await?;
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

    /// Frame `n`'s time on the track, which is the recording's clock: the
    /// frame ends here, as a show export's frame ends on its tick. In `f64`,
    /// so one frame's slice of the clock ends exactly where the next opens.
    fn time(&self, n: u64) -> f64 {
        f64::from(self.start) + n as f64 / f64::from(self.fps)
    }

    /// The frame that ends at `time`, with the track's bass at `bass`: one
    /// moment, as live draws it ([`footage::moment`]). Its strobes flash when
    /// a flash begins in the frame interval before it.
    fn moment(&self, time: f64, bass: f32) -> Moment {
        footage::moment(time, 1.0 / f64::from(self.fps), bass)
    }

    /// The moments drawn and discarded before frame zero, in order, so that the
    /// renderer's haze history is converged by the time the span opens.
    ///
    /// They run up to the start on the output grid, so the last of them is one
    /// frame interval before the first kept frame and the history stays
    /// continuous across the join. Clamped at zero: a recording that starts at
    /// the top of the track warms up on its own first moment, which converges
    /// the haze without inventing a `t` the score has no state for.
    fn warmup(&self) -> impl Iterator<Item = f64> + use<> {
        let (start, fps) = (f64::from(self.start), f64::from(self.fps));
        (0..WARMUP_FRAMES).map(move |i| (start - (WARMUP_FRAMES - i) as f64 / fps).max(0.0))
    }

    /// Every moment drawn, in order, and whether it goes into the file: the
    /// warm-up, then the span.
    fn ticks(&self) -> impl Iterator<Item = (f64, bool)> + use<> {
        let axis = *self;
        self.warmup()
            .map(|time| (time, false))
            .chain((0..axis.frames).map(move |n| (axis.time(n), true)))
    }
}

// ---------------------------------------------------------------------------
// the encoder
// ---------------------------------------------------------------------------

/// Everything the ffmpeg argv is a function of.
///
/// Frames are sRGB at ten bits a channel, as [`Recorder`] reads them back: a
/// little-endian 32-bit word a pixel, red in the low ten bits, then green,
/// blue and two bits of alpha — ffmpeg's `x2bgr10le`, which names the fields
/// from the most significant bit down.
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
            // gradients, which 8-bit bands and plain CQ starves of bits. The
            // slowest preset and a low CQ keep the darks from breaking into
            // blocks; NVENC is still faster than the renderer at p7.
            // `hvc1` so Apple players open the file too.
            Self::Nvenc => &[
                "-c:v",
                "hevc_nvenc",
                "-preset",
                "p7",
                "-tune",
                "hq",
                "-rc",
                "vbr",
                "-cq",
                "12",
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
            // Still 8-bit 4:2:0, which every player decodes. `aq-mode=3`
            // biases the adaptive quantiser toward dark blocks, where the
            // default starves them; `slow` and a lower CRF pay for the rest.
            Self::X264 => &[
                "-c:v",
                "libx264",
                "-preset",
                "slow",
                "-crf",
                "14",
                "-x264-params",
                "aq-mode=3",
            ],
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
    let bundled = crate::runtime::ffmpeg::ffmpeg_path();
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
        "x2bgr10le".into(),
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

/// A running ffmpeg writing raw frames to an mp4.
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
    settings: crate::settings::AppSettings,
    meshes_root: PathBuf,
    aim: Aim,
    size: (u32, u32),
    audio: PathBuf,
    /// The decoded track, for the footage look's bass shake.
    bass: TrackAudio,
    output: PathBuf,
}

impl Session {
    async fn prepare(services: &AppServices, spec: Recording) -> Result<Self, RecordError> {
        let (pool, storage, fixtures_root) = (
            &services.db().0,
            services.storage(),
            services.fixtures_root(),
        );
        let (track_id, venue_id) = score_scope(pool, &spec.score_id).await?;

        let duration = crate::database::local::tracks::get_track_duration(pool, &track_id)
            .await?
            .ok_or(RecordError::NoDuration)? as f32;
        let axis = TimeAxis::new(spec.span, duration, spec.fps)?;

        let audio = services
            .track_audio
            .ensure(&track_id)
            .await
            .map_err(|error| RecordError::NoAudio(error.to_string()))?;
        // Decoded through the cache playback fills, so a track the stage has
        // played is not decoded twice.
        let hash = crate::database::local::tracks::get_track_path_and_hash(pool, &track_id)
            .await?
            .track_hash;
        let path = audio.clone();
        let samples = tokio::task::spawn_blocking(move || {
            crate::audio::load_or_decode_audio_shared(&path, &hash)
        })
        .await
        .map_err(|error| RecordError::Library(format!("the decode failed: {error}")))??;

        let lighting = crate::compositor::build_score_scene(
            pool,
            storage,
            fixtures_root,
            &spec.score_id,
            spec.score,
        )
        .await?;
        if lighting.annotations.is_empty() {
            return Err(RecordError::EmptyScore(spec.score_id));
        }

        let geometry = VenueGeometry::load(pool, fixtures_root, &venue_id).await?;
        if geometry.is_empty() {
            return Err(RecordError::EmptyVenue);
        }
        let settings = crate::settings::load_settings(pool).await?;

        Ok(Self {
            axis,
            lighting,
            geometry,
            settings,
            meshes_root: stage_render::meshes_root(Some(fixtures_root)),
            aim: spec.aim,
            size: (spec.size.0.max(1), spec.size.1.max(1)),
            audio,
            bass: TrackAudio::whole(samples),
            output: spec.output,
        })
    }

    /// The frame loop. Blocks for the length of the render.
    fn run(
        self,
        cancel: &AtomicBool,
        progress: &(dyn Fn(Progress) + Send),
    ) -> Result<Recorded, RecordError> {
        let (width, height) = self.size;
        let (mut scene, definitions) = self.geometry.show_scene(&self.settings);
        let camera = self
            .aim
            .camera(&scene, &definitions, self.geometry.booth(), self.size);
        scene.camera.position = coords::three_from_world(camera.position()).to_array();
        scene.camera.target = coords::three_from_world(camera.target).to_array();
        scene.render.fov = camera.fov_y_deg;

        let mut encoder = Encoder::start(&Encode {
            size: self.size,
            fps: self.axis.fps,
            audio: Some((&self.audio, self.axis.start, self.axis.seconds())),
            output: &self.output,
        })?;
        let mut recorder = Recorder::new()
            .map_err(|error| RecordError::Library(format!("no GPU for the recording: {error}")))?;
        let mut assets = assets::Library::new(self.meshes_root.clone());
        let mut motors = Motors::default();
        let mut arena = Arena::default();

        let started = Instant::now();
        let (mut render, mut encode) = (Duration::ZERO, Duration::ZERO);
        let mut written = 0u64;
        let mut cancelled = false;
        // Whether each frame in flight is kept, oldest first. The recorder
        // hands frames back in the order they went in.
        let mut in_flight = VecDeque::new();
        let mut keep =
            |kept: Option<bool>, pixels: Vec<u8>, render: Duration| -> Result<(), RecordError> {
                if kept != Some(true) {
                    return Ok(());
                }
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
                Ok(())
            };
        for (time, kept) in self.axis.ticks() {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let clock = Instant::now();
            // The light state at the frame's track time, turned by the motors
            // and drawn at its moment, as the stage draws it.
            let score = time as f32;
            let state = self
                .lighting
                .render(&[score], Scope::Composite, &mut arena)
                .into_iter()
                .next();
            let universe = motors.step(time, score, state);
            let moment = self.axis.moment(time, self.bass.bass_at(score));
            let mut frame = build_frame_at(
                &scene,
                &definitions,
                &|id, head| stage_render::primitive_state(universe.as_ref(), id, head),
                moment,
                &mut assets,
            )
            .map_err(|error| {
                RecordError::Library(format!("could not assemble the frame: {error}"))
            })?;
            if let Some(state) = &universe {
                frame.lasers = stage_render::laser_scans(&self.geometry.lasers, state, time);
            }
            in_flight.push_back(kept);
            let done = recorder.push(&frame, width, height).map_err(|error| {
                RecordError::Library(format!("could not render the frame: {error}"))
            })?;
            render += clock.elapsed();
            if let Some(pixels) = done {
                keep(in_flight.pop_front(), pixels, render)?;
            }
        }
        while let Some(pixels) = recorder
            .pop()
            .map_err(|error| RecordError::Library(format!("could not render the frame: {error}")))?
        {
            keep(in_flight.pop_front(), pixels, render)?;
        }
        drop(keep);
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
        let axis = TimeAxis::new(None, 10.0, 30).expect("a ten second track records");
        assert_eq!(axis.frames(), 300);
        assert!((axis.time(0) - 0.0).abs() < 1e-6);
        // The last frame is one interval before the end, not at it.
        assert!((axis.time(299) - (10.0 - 1.0 / 30.0)).abs() < 1e-5);
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
        assert!((axis.time(0) - 30.0).abs() < 1e-5);
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
    fn a_frame_is_one_moment_whose_slice_meets_the_next() {
        let axis = TimeAxis::new(Some((30.0, 40.0)), 200.0, 30).unwrap();
        for n in [0u64, 1, 299] {
            let moment = axis.moment(axis.time(n), 0.0);
            // The frame ends on its time, and its strobes answer for the whole
            // interval since the frame before.
            assert_eq!(moment.time, axis.time(n));
            assert!((moment.slice.open + moment.slice.length - moment.time).abs() < 1e-12);
            assert!((moment.slice.length - 1.0 / 30.0).abs() < 1e-12);
            let next = axis.moment(axis.time(n + 1), 0.0).slice;
            assert!((next.open - moment.time).abs() < 1e-12);
        }
    }

    #[test]
    fn the_warm_up_lands_on_the_output_grid() {
        let axis = TimeAxis::new(Some((30.0, 40.0)), 200.0, 30).unwrap();
        let warm: Vec<f64> = axis.warmup().collect();
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
        let axis = TimeAxis::new(None, 10.0, 30).unwrap();
        assert!(axis.warmup().all(|t| t >= 0.0));
    }

    #[test]
    fn every_frame_of_the_span_is_kept_once_after_the_warm_up() {
        let axis = TimeAxis::new(Some((2.0, 3.0)), 10.0, 30).unwrap();
        let ticks: Vec<(f64, bool)> = axis.ticks().collect();
        let warm = WARMUP_FRAMES as usize;
        assert_eq!(ticks.len(), warm + 30);
        assert!(ticks[..warm].iter().all(|&(_, kept)| !kept));
        let kept: Vec<f64> = ticks[warm..].iter().map(|&(time, _)| time).collect();
        assert!(ticks[warm..].iter().all(|&(_, kept)| kept));
        assert_eq!(kept, (0..30).map(|n| axis.time(n)).collect::<Vec<_>>());
    }

    #[test]
    fn the_bass_reaches_the_moment() {
        let axis = TimeAxis::new(None, 10.0, 30).unwrap();
        assert_eq!(axis.moment(1.0, 0.7).bass, 0.7);
        assert_eq!(axis.moment(1.0, 0.0).bass, 0.0);
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
        assert!(line.contains("-f rawvideo -pix_fmt x2bgr10le -s 1280x720 -r 30 -i pipe:0"));
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
        let line = argv.join(" ");
        assert!(line.contains("-f rawvideo -pix_fmt x2bgr10le -s 640x360"));
        assert!(line.contains("-profile:v main10"));
        assert!(line.contains("format=p010le"));
    }

    /// Encode a flat frame of one 10-bit colour with `codec`, decode the
    /// file, and hand back the colour it decodes to, in 10-bit codes.
    fn round_trip(ffmpeg: &Path, codec: Codec, colour: [u32; 3]) -> [f32; 3] {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("flat.mp4");
        // NVENC's smallest frame is larger than 64x64.
        let (width, height) = (256, 256);
        let encode = Encode {
            size: (width, height),
            fps: 30,
            audio: None,
            output: &output,
        };
        let word = colour[0] | colour[1] << 10 | colour[2] << 20 | 3 << 30;
        let frame = word.to_le_bytes().repeat((width * height) as usize);
        let mut child = Command::new(ffmpeg)
            .args(ffmpeg_argv(&encode, codec))
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        for _ in 0..4 {
            stdin.write_all(&frame).unwrap();
        }
        drop(stdin);
        let encoded = child.wait_with_output().unwrap();
        assert!(
            encoded.status.success(),
            "{codec:?}: {}",
            String::from_utf8_lossy(&encoded.stderr)
        );
        let decoded = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&output)
            .args([
                "-vf",
                "scale=in_color_matrix=bt709:in_range=tv,format=rgb48le",
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "pipe:1",
            ])
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let centre = ((height / 2 * width + width / 2) * 6) as usize;
        std::array::from_fn(|channel| {
            let at = centre + channel * 2;
            let value = u16::from_le_bytes([decoded.stdout[at], decoded.stdout[at + 1]]);
            f32::from(value) / 65535.0 * 1023.0
        })
    }

    #[test]
    fn ten_bit_frames_decode_to_their_own_colour() {
        // Dark, a different value per channel, and off the 8-bit grid (a
        // multiple of four would survive an 8-bit path unchanged).
        const COLOUR: [u32; 3] = [201, 90, 42];
        // The bundled ffmpeg the build downloads, as every host uses; a
        // runner need not have one on PATH.
        crate::runtime::ffmpeg::init_headless();
        let (ffmpeg, codec) = pick_encoder();
        // x264 is 8-bit: the colour survives, its two low bits do not.
        let mut codecs = vec![(Codec::X264, 2.0 * 1023.0 / 255.0)];
        if codec == Codec::Nvenc {
            // Main10 keeps steps finer than one 8-bit code.
            codecs.push((Codec::Nvenc, 1.5));
        }
        for (codec, tolerance) in codecs {
            let decoded = round_trip(&ffmpeg, codec, COLOUR);
            for (got, want) in decoded.into_iter().zip(COLOUR) {
                assert!(
                    (got - want as f32).abs() <= tolerance,
                    "{codec:?} decoded {decoded:?}, wanted {COLOUR:?}"
                );
            }
        }
    }

    #[test]
    fn videotoolbox_is_rate_controlled_by_the_frame_size() {
        // 1920x1080x30 at 0.15 bpp.
        assert!(Codec::VideoToolbox
            .args((1920, 1080), 30)
            .contains(&"9331200".to_string()));
    }
}
