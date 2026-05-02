//! Offline video export pipeline.
//!
//! Frontend renders each frame into a WebGLRenderTarget, pipes it through the
//! WebCodecs `VideoEncoder` (hardware H.264), then ships the ~20KB encoded
//! chunks to us. We pipe those directly into ffmpeg with `-c:v copy` so there
//! is no second CPU encode — the entire video path stays on the GPU until it
//! hits the MP4 muxer. Audio is muxed from the original track file.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tauri::State;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::compositor::get_track_duration;
use crate::database::Db;
use crate::engine::render_frame_max;
use crate::ffmpeg_env::ffmpeg_path;
use crate::models::node_graph::LayerTimeSeries;
use crate::models::universe::UniverseState;
use crate::render_engine::RenderEngine;

pub struct ExportSession {
    layer: LayerTimeSeries,
    ffmpeg: Child,
    stdin: ChildStdin,
    output_path: PathBuf,
    fps: u32,
    chunks_written: u64,
    bytes_written: u64,
    // Diagnostic counters — logged every 30 chunks to trace bottlenecks.
    sample_ns: Arc<AtomicU64>,
    push_wait_ns: Arc<AtomicU64>,
    push_write_ns: Arc<AtomicU64>,
}

#[derive(Default, Clone)]
pub struct ExportSessionsState {
    sessions: Arc<Mutex<HashMap<String, ExportSession>>>,
}

impl ExportSessionsState {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../src/bindings/export.ts")]
#[serde(rename_all = "camelCase")]
pub struct ExportStarted {
    pub session_id: String,
    pub total_frames: u64,
    pub duration_seconds: f32,
}

#[tauri::command]
pub async fn export_start(
    db: State<'_, Db>,
    render_engine: State<'_, RenderEngine>,
    sessions: State<'_, ExportSessionsState>,
    track_id: String,
    output_path: String,
    fps: u32,
    width: u32,
    height: u32,
) -> Result<ExportStarted, String> {
    if !(1..=240).contains(&fps) {
        return Err(format!("fps must be 1..=240, got {fps}"));
    }
    if width == 0 || height == 0 || width > 7680 || height > 4320 {
        return Err(format!("invalid resolution {width}x{height}"));
    }

    let layer = render_engine
        .get_active_layer()
        .ok_or_else(|| "No active composite layer. Call composite_track first.".to_string())?;

    let duration = get_track_duration(&db.0, &track_id)
        .await?
        .ok_or_else(|| format!("Track {track_id} has no duration"))?;

    let track_info =
        crate::database::local::tracks::get_track_path_and_hash(&db.0, &track_id).await?;
    let audio_path = PathBuf::from(&track_info.file_path);
    if !audio_path.exists() {
        return Err(format!(
            "Track audio file missing: {}",
            audio_path.display()
        ));
    }

    let output_pathbuf = PathBuf::from(&output_path);
    if let Some(parent) = output_pathbuf.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(format!(
                "Output directory does not exist: {}",
                parent.display()
            ));
        }
    }

    let total_frames = (duration as f64 * fps as f64).ceil() as u64;

    // ffmpeg reads an Annex-B H.264 stream from stdin (pre-encoded by the
    // browser's hardware VideoEncoder) and stream-copies it into an MP4 with
    // the track audio muxed in.
    let mut cmd = Command::new(ffmpeg_path());
    cmd.arg("-y")
        .arg("-f")
        .arg("h264")
        .arg("-r")
        .arg(fps.to_string())
        .arg("-i")
        .arg("pipe:0")
        .arg("-i")
        .arg(&audio_path)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("1:a:0")
        .arg("-c:v")
        .arg("copy")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("192k")
        .arg("-shortest")
        .arg("-movflags")
        .arg("+faststart")
        .arg(&output_pathbuf)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn ffmpeg: {e}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "ffmpeg stdin was not captured".to_string())?;

    let session_id = Uuid::new_v4().to_string();
    let session = ExportSession {
        layer,
        ffmpeg: child,
        stdin,
        output_path: output_pathbuf,
        fps,
        chunks_written: 0,
        bytes_written: 0,
        sample_ns: Arc::new(AtomicU64::new(0)),
        push_wait_ns: Arc::new(AtomicU64::new(0)),
        push_write_ns: Arc::new(AtomicU64::new(0)),
    };

    sessions
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    Ok(ExportStarted {
        session_id,
        total_frames,
        duration_seconds: duration,
    })
}

#[tauri::command]
pub async fn export_sample_frame(
    sessions: State<'_, ExportSessionsState>,
    session_id: String,
    t_seconds: f32,
) -> Result<UniverseState, String> {
    let t0 = Instant::now();
    let guard = sessions.sessions.lock().await;
    let session = guard
        .get(&session_id)
        .ok_or_else(|| format!("Unknown export session {session_id}"))?;
    let dt = 1.0 / session.fps as f32;
    let t_prev = (t_seconds - dt).max(0.0);
    let result = render_frame_max(&session.layer, t_prev, t_seconds);
    session
        .sample_ns
        .fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    Ok(result)
}

/// Sample a contiguous range of frames in a single IPC call. Amortises the
/// per-invoke overhead (which was dominating the export loop).
#[tauri::command]
pub async fn export_sample_batch(
    sessions: State<'_, ExportSessionsState>,
    session_id: String,
    start_frame: u32,
    count: u32,
) -> Result<Vec<UniverseState>, String> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > 600 {
        return Err(format!("batch too large: {count} (max 600)"));
    }
    let t0 = Instant::now();
    let guard = sessions.sessions.lock().await;
    let session = guard
        .get(&session_id)
        .ok_or_else(|| format!("Unknown export session {session_id}"))?;
    let dt = 1.0 / session.fps as f32;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let t = (start_frame + i) as f32 * dt;
        let t_prev = (t - dt).max(0.0);
        out.push(render_frame_max(&session.layer, t_prev, t));
    }
    session
        .sample_ns
        .fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    Ok(out)
}

/// Push an encoded H.264 chunk (Annex-B) to ffmpeg. The chunk arrives as the
/// raw invoke body and the session id comes via header; this avoids any JSON
/// encoding of the payload.
#[tauri::command]
pub async fn export_push_chunk(
    sessions: State<'_, ExportSessionsState>,
    request: tauri::ipc::Request<'_>,
) -> Result<u64, String> {
    let session_id = request
        .headers()
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| "Missing x-session-id header".to_string())?
        .to_string();

    let chunk: &[u8] = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.as_slice(),
        tauri::ipc::InvokeBody::Json(_) => {
            return Err(
                "export_push_chunk requires raw body; pass Uint8Array as invoke args".to_string(),
            );
        }
    };

    let wait_start = Instant::now();
    let mut guard = sessions.sessions.lock().await;
    let session = guard
        .get_mut(&session_id)
        .ok_or_else(|| format!("Unknown export session {session_id}"))?;
    let wait_ns = wait_start.elapsed().as_nanos() as u64;

    let write_start = Instant::now();
    session
        .stdin
        .write_all(chunk)
        .await
        .map_err(|e| format!("Failed to write chunk to ffmpeg: {e}"))?;
    let write_ns = write_start.elapsed().as_nanos() as u64;

    session.chunks_written += 1;
    session.bytes_written += chunk.len() as u64;
    session.push_wait_ns.fetch_add(wait_ns, Ordering::Relaxed);
    session.push_write_ns.fetch_add(write_ns, Ordering::Relaxed);

    if session.chunks_written % 30 == 0 {
        let n = 30u64;
        let sample = session.sample_ns.swap(0, Ordering::Relaxed) as f64 / n as f64 / 1e6;
        let wait = session.push_wait_ns.swap(0, Ordering::Relaxed) as f64 / n as f64 / 1e6;
        let write = session.push_write_ns.swap(0, Ordering::Relaxed) as f64 / n as f64 / 1e6;
        eprintln!(
            "[export.rs] chunk {} bytes={} total={:.1}MB | avg ms: sample={:.2} push_lock_wait={:.2} ffmpeg_write={:.3}",
            session.chunks_written,
            chunk.len(),
            session.bytes_written as f64 / 1_048_576.0,
            sample,
            wait,
            write
        );
    }

    Ok(session.chunks_written)
}

#[tauri::command]
pub async fn export_finish(
    sessions: State<'_, ExportSessionsState>,
    session_id: String,
) -> Result<String, String> {
    let mut session = {
        let mut guard = sessions.sessions.lock().await;
        guard
            .remove(&session_id)
            .ok_or_else(|| format!("Unknown export session {session_id}"))?
    };
    session
        .stdin
        .shutdown()
        .await
        .map_err(|e| format!("Failed to close ffmpeg stdin: {e}"))?;
    drop(session.stdin);
    let status = session
        .ffmpeg
        .wait()
        .await
        .map_err(|e| format!("ffmpeg wait failed: {e}"))?;
    if !status.success() {
        return Err(format!(
            "ffmpeg exited with status {status} after {} chunks ({} bytes)",
            session.chunks_written, session.bytes_written
        ));
    }
    Ok(session.output_path.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn export_cancel(
    sessions: State<'_, ExportSessionsState>,
    session_id: String,
) -> Result<(), String> {
    let mut session = {
        let mut guard = sessions.sessions.lock().await;
        match guard.remove(&session_id) {
            Some(s) => s,
            None => return Ok(()),
        }
    };
    let _ = session.ffmpeg.start_kill();
    let _ = session.ffmpeg.wait().await;
    Ok(())
}
