# Recording a score as video

`luma_lib::recording` (`backend/src/recording.rs`) is the one way a score
becomes a video. It has two callers, the `luma-cli record` command and the
app's "Export show" dialog. Each fills in a `Recording`, calls
`recording::record`, and waits.

The module owns the time axis and the container: span clamping, the frame
count, the warm-up, the encoder choice, the audio cut and the ffmpeg argv. It
draws each frame the way the stage does: the same score compile, the same motor
lag, the same bass under the footage look, and the live pass chain. Scene
assembly happens once. The venue, the score and the audio are read before frame
zero, and each frame carries only its light state and its moment.

---

## 1. The request

```rust
// backend/src/recording.rs
pub struct Recording {
    pub score_id: String,                     // its track and venue come with it
    pub score: Option<luma_patterns::Score>,  // the editor's working copy, if any
    pub aim: Aim,
    pub span: Option<(f32, f32)>,             // seconds; None is the whole track
    pub size: (u32, u32),
    pub fps: u32,
    pub output: PathBuf,
}

pub async fn record(
    services: &AppServices,
    spec: Recording,
    cancel: CancelFlag,                       // Arc<AtomicBool>
    progress: impl Fn(Progress) + Send + 'static,
) -> Result<Recorded, RecordError>;
```

`record` prepares the session on the async side, then runs the frame loop on a
blocking-pool thread. `cancel` is polled once per frame. `progress` is called
once per frame written, with the frame number, the total, the wall time since
the loop started, and the cumulative render and encode times. `Recorded`
carries the path, the frames written, the seconds of video (`frames / fps`) and
the same three times.

`RecordError` names what can go wrong: no such score, a score with no clips, a
venue with no patched fixtures and no stage pieces, a track with no known
duration, a span that is empty or outside the track, audio that cannot be
fetched, ffmpeg that cannot be started, a broken pipe, a non-zero ffmpeg exit
(with its stderr), and cancellation.

`stage_render::Aim` is where the recording is seen from:

- `Aim::View(View)` is a named view, fitted to the rig at the frame's aspect
  ratio.
- `Aim::Camera(Camera)` is a camera exactly as the stage holds it.

Recordings are not capped in size. `stage_render::MAX_DIMENSION` is the cap on
an agent's stills and applies to stills only.

---

## 2. What is read once

`Session::prepare` reads everything before the first frame:

1. The score's `(track_id, venue_id)`, from the `scores` row.
2. The track's duration, which with `span` and `fps` makes the time axis (§3).
3. The track's audio, through `TrackAudio::ensure` — the fetch playback and the
   waveform use, so a track whose audio lives on another device is downloaded
   rather than refused. The file is then decoded through the cache playback
   fills (`audio::load_or_decode_audio_shared`), for the bass shake.
4. The lighting, through `compositor::build_score_scene`, with the caller's
   working copy of the score when it has one. A score with no annotations is
   `EmptyScore`.
5. The venue, through `VenueGeometry::load`. An empty one is `EmptyVenue`.
6. The app settings, for the stage look.

The scene that is drawn is `VenueGeometry::show_scene`: the look this device's
stage uses, at `Quality::High`, with geometry shadows on and the editor's grid
and gizmos off. The camera is resolved from the `Aim` once and written into the
scene; framing does not depend on `t`.

---

## 3. The time axis

`TimeAxis` is the grid of moments one recording samples.

- **Span.** `None` is the whole track. A span is clamped into
  `[0, duration]` rather than refused. The only error is a span that clamps to
  nothing (reversed, outside the track, or not finite).
- **Frame count.** `round((end - start) * fps)`, so a span of exactly 10 s at
  30 fps is 300 frames. `fps` is clamped to at least 1.
- **Frame time.** Frame `n` ends at `start + n / fps` on the track, computed in
  `f64` so one frame's slice of the clock ends exactly where the next opens.
- **Moment.** Each frame is one moment, as live draws it
  (`luma_render::footage::moment`): its time, the slice of the clock since the
  frame before it, and the track's bass at that time. A strobing light shows
  the share of the slice its gate is on (`luma_scene::strobe`), so consecutive
  frames add up to the light the rig gives whatever the frame rate.
- **Warm-up.** `WARMUP_FRAMES = 24` frames are drawn and discarded before the
  span starts, so what the live pass chain carries from frame to frame has
  settled before the first kept frame. The warm-up moments run up to the start
  on the output grid, so that history is continuous across the join. They are
  clamped at zero: a recording that starts at the top of the track warms up on
  its own first moment.

---

## 4. Drawing a frame

For each moment, warm-up then span, `Session::run`:

1. evaluates the lighting at the frame's track time
   (`Scene::try_render(&[t], Scope::Composite, &mut arena)`);
2. steps the motors (`luma_lib::motors`, the stage's own preview motor lag);
3. builds the frame with `luma_render::build_frame_at`, reading each head's
   light through `StageLights::frame(state, slice).primitive(id, head)`, as the
   rig makes it, and adds the laser scans;
4. pushes it to a `luma_render::Recorder`.

`Recorder` (`gpui/crates/render/src/viewport.rs`) draws live-quality frames for
a file: the same pass chain, `LIVE_SUBFRAMES` budget and frame-to-frame history
as the viewport, on a renderer of its own so that history is not the
viewport's. Nothing is paced to a display. Up to `RECORDER_DEPTH = 3` frames
are in flight, each in its own presentation slot, and they come back in the
order they went in. Frames are read back as sRGB at ten bits a channel:
little-endian 32-bit words with red in the low ten bits, then green, blue and
two bits of alpha, which is ffmpeg's `x2bgr10le`.

Warm-up frames go through the same loop and are dropped when they come back.

---

## 5. Encoding

`Encoder` runs one ffmpeg and writes raw frames to its stdin. The argv is fixed
by the module (`ffmpeg_argv`), never by a caller:

```
ffmpeg -y -hide_banner -loglevel error
  -f rawvideo -pix_fmt x2bgr10le -s {W}x{H} -r {fps} -i pipe:0
  [-ss {start}] -t {frames/fps} -i {audio}
  -map 0:v:0 -map 1:a:0 -c:a aac -b:a 192k
  {codec args}
  -vf scale=out_color_matrix=bt709:out_range=tv,format={pixel format}
  -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv
  -movflags +faststart
  {output}
```

- **Audio.** `t` is the audio file's own position, so only a span that does not
  start at zero needs `-ss`. The audio is cut to the video's length with `-t`,
  not with `-shortest`: `-shortest` finalises the file the moment the video
  pipe reaches EOF, and behind a slow pipe the audio decoder is seconds behind
  when that happens, which loses the end of the audio.
- **Colour.** The frames are sRGB. They are converted to YUV with the BT.709
  matrix and tagged BT.709, limited range. Without the explicit matrix swscale
  converts with BT.601 and the tag names a matrix the pixels were not made
  with.
- **Codec** (`pick_encoder`):
  - macOS: `h264_videotoolbox` from the bundled ffmpeg, rate-controlled at
    `BITS_PER_PIXEL = 0.15` bits per pixel per second, `yuv420p`.
  - Linux and Windows: `hevc_nvenc` (Main10, `p010le`, tagged `hvc1`) when the
    bundled ffmpeg or the one on `PATH` can open it. Being listed in
    `-encoders` is not enough, so one black frame is encoded to find out.
  - Otherwise `libx264 -preset slow -crf 14 -x264-params aq-mode=3`,
    `yuv420p`.
- **Pipe.** Frames cross to ffmpeg on a thread of their own through a queue of
  `ENCODE_QUEUE = 2`, so the next frame renders while the last is in the pipe
  and a slow encoder holds the renderer back rather than buffering the film in
  memory. ffmpeg's stderr is drained on another thread and returned in the
  error when it exits non-zero.
- **The file.** A failed encode deletes the partial file. An `Encoder` dropped
  before `finish` kills ffmpeg and deletes it too. A cancelled recording stops
  at the next frame boundary and is closed the ordinary way, so what was
  rendered is a playable file; `record` then returns `RecordError::Cancelled`.

The bundled ffmpeg is found by `runtime::ffmpeg::ffmpeg_path()`; a headless
binary sets the search up with `runtime::ffmpeg::init_headless()`.

The mp4 is not byte-reproducible. Compare decoded frames.

---

## 6. The callers

### `luma-cli record`

`backend/src/bin/luma-cli/record.rs` parses flags, boots the same headless host
`luma-cli rpc` and `luma-cli mcp` boot, and prints progress.

```
luma-cli record <score-id> <out.mp4>
    [--view front|audience|overhead|quarter-left|quarter-right|dj]
    [--camera EXPORT.json]
    [--width 1280] [--height 720] [--fps 30] [--span start:end]
    [--config-dir DIR] [--fixtures-root DIR] [--cache-dir DIR]
```

The defaults are 1280×720 at 30 fps from the front view, over the whole track.
`--span` takes seconds; either side may be empty for the track's own start or
end. `--camera` reads the camera from a file the stage's "Export camera" action
wrote, and records through it as `Aim::Camera`. Flags it does not know are
forwarded to `HostConfig::parse_args`.

Progress is a bar on stderr. Ctrl-C stops at the next frame. On success the
summary line (frames, realtime factor, render and encode ms per frame) goes to
stderr and the output path alone goes to stdout.

### Export show

`gpui/crates/app/src/export_dialog.rs` is the dialog the track editor's
"Export show" button raises. The button is enabled once the track's length is
known.

- **Size.** 1080p, 1440p or 4K, always 16:9. It opens on 4K.
- **Frame rate.** 60 fps.
- **Span.** The whole track.
- **File.** `~/Movies/<track name>.mp4` on macOS, `~/Videos/<track name>.mp4`
  elsewhere, or a path chosen in the platform's save dialog.
- **Camera.** The stage's camera, as `Aim::Camera`. With no stage open, Start
  is refused and the dialog says to open the stage.
- **Score.** The editor's working copy, unsaved edits included, so the file
  lights what the stage lights.

The dialog starts the recording through `Library::record`
(`gpui/crates/app/src/library/render.rs`), which calls `recording::record`
directly. It shows frames written against the total as they land. Cancel sets
the cancel flag. A failure shows as failed in the dialog, with the cause in the
log.

### Stills

A still (`venue.render`, `stage_render::render_rgba`) is not a recording. It
goes through `stage_render::Sequence` and `Renderer::render`, whose image is a
function of the one frame alone.
