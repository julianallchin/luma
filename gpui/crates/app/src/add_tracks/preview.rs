//! Auditioning a source row before importing it.
//!
//! One preview at a time, on the app's one player — the same host session
//! machinery the track editor plays through ([`crate::Library::load_file_audio`]),
//! so a preview and a song cannot sound together: starting one parks the
//! editor's audio, and stopping one hands it back.
//!
//! # When a preview ends
//!
//! A preview belongs to one dialog, on the source route, browsing the source
//! it was started in. [`reap_preview`] checks exactly that on every frame and
//! stops a preview that has lost any of the three — which is what makes
//! "stops when the dialog closes, the source changes, or an import starts"
//! one rule rather than a call remembered at each of those places (an import
//! moves the dialog to its progress route; a source change bumps the source
//! generation; and a dialog can go away by more paths than one).

use std::rc::Rc;
use std::time::Duration;

use gpui::{Context, Task};

use super::{AddTracks, Route};
use crate::Luma;

/// Where a preview starts, as a fraction of the track: past the intro, into
/// the body of the song.
pub(crate) const PREVIEW_START: f32 = 0.35;
/// Shift+←/→.
pub(crate) const SEEK_STEP: f32 = 10.0;
/// How often a playing preview reads the transport. The scrub bar is a few
/// hundred pixels for minutes of audio, so this is several frames per pixel.
const POLL: Duration = Duration::from_millis(100);

pub(crate) struct SourcePreview {
    /// The dialog instance, and the source generation inside it, this
    /// preview was started from.
    pub(super) dialog: uuid::Uuid,
    pub(super) source_generation: u64,
    pub(super) track_id: String,
    session: u64,
    /// `None` while the file decodes.
    pub(super) loaded: Option<Loaded>,
    pub(super) playing: bool,
    /// Playhead, in seconds of the file.
    pub(super) position: f32,
    /// A pointer is dragging the scrub bar: the transport's own position is
    /// stale until it lets go, so polls do not move the playhead meanwhile.
    pub(super) scrubbing: bool,
    /// Why the file would not play, shown in place of the scrub bar.
    pub(super) failed: Option<String>,
    _poll: Option<Task<()>>,
}

pub(super) struct Loaded {
    pub(super) duration: f32,
    /// Peaks in `0..=1`, start to end — the scrub bar's waveform.
    pub(super) overview: Rc<[f32]>,
}

impl SourcePreview {
    pub(super) fn fraction(&self) -> f32 {
        match &self.loaded {
            Some(loaded) if loaded.duration > 0.0 => {
                (self.position / loaded.duration).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }
}

impl Luma {
    fn open_add_tracks_state(&self) -> Option<&AddTracks> {
        match self.overlay.as_open() {
            Some(crate::shell::Overlay::AddTracks(state)) => Some(state),
            _ => None,
        }
    }

    /// Play `track_id`, or pause / resume it if it is the row already
    /// previewing. Starting a row stops whichever row was playing.
    pub(crate) fn toggle_source_preview(&mut self, track_id: &str, cx: &mut Context<Self>) {
        if let Some(preview) = self
            .source_preview
            .as_mut()
            .filter(|preview| preview.track_id == track_id && preview.failed.is_none())
        {
            let Some(loaded) = &preview.loaded else {
                // Still decoding: a second press means "never mind".
                self.stop_source_preview(cx);
                return;
            };
            let session = preview.session;
            if preview.playing {
                preview.playing = false;
                let pause = self.library.pause(session);
                sent(pause);
            } else {
                // Finished playing: a press plays it again from the top.
                if preview.position >= loaded.duration - 0.05 {
                    preview.position = 0.0;
                }
                preview.playing = true;
                let play = self.library.play(session, preview.position);
                sent(play);
                self.poll_source_preview(session, cx);
            }
            cx.notify();
            return;
        }
        self.start_source_preview(track_id, cx);
    }

    fn start_source_preview(&mut self, track_id: &str, cx: &mut Context<Self>) {
        let Some(state) = self.open_add_tracks_state() else {
            return;
        };
        let Some(track) = state
            .source_rows
            .iter()
            .find(|track| track.id == track_id)
            .cloned()
        else {
            return;
        };
        let (dialog, source_generation) = (state.instance_id, state.source_generation);
        // Handing the player from one preview to the next, not back to the
        // editor — giving the editor its audio only to take it away again
        // would leave it holding a session that is already gone.
        self.end_source_preview();
        self.park_track_audio(None, cx);
        let mut preview = SourcePreview {
            dialog,
            source_generation,
            track_id: track.id.clone(),
            session: 0,
            loaded: None,
            playing: false,
            position: 0.0,
            scrubbing: false,
            failed: None,
            _poll: None,
        };
        let Some(path) = track.file_path.clone() else {
            preview.failed = Some("No audio file".to_string());
            self.source_preview = Some(preview);
            cx.notify();
            return;
        };
        let (session, pending) = self.library.load_file_audio(&path);
        preview.session = session;
        self.source_preview = Some(preview);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            let play = this.update(cx, |this, cx| {
                let preview = this
                    .source_preview
                    .as_mut()
                    .filter(|preview| preview.session == session)?;
                cx.notify();
                match result {
                    Ok(Some(file)) => {
                        preview.position = file.duration_seconds * PREVIEW_START;
                        preview.playing = true;
                        preview.loaded = Some(Loaded {
                            duration: file.duration_seconds,
                            overview: file.overview.into(),
                        });
                        Some(this.library.play(session, preview.position))
                    }
                    // A newer load took the player first.
                    Ok(None) => None,
                    Err(error) => {
                        preview.failed = Some(error.to_string());
                        None
                    }
                }
            });
            let Ok(Some(play)) = play else {
                return;
            };
            let result = play.await;
            this.update(cx, |this, cx| {
                let Some(preview) = this
                    .source_preview
                    .as_mut()
                    .filter(|preview| preview.session == session)
                else {
                    return;
                };
                match result {
                    Ok(()) => this.poll_source_preview(session, cx),
                    Err(error) => {
                        preview.playing = false;
                        preview.failed = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Follow the transport while `session` is the preview. Ends when the
    /// preview does, or when something else takes the player — then the
    /// preview is over, whoever ended it.
    fn poll_source_preview(&mut self, session: u64, cx: &mut Context<Self>) {
        let Some(preview) = self
            .source_preview
            .as_mut()
            .filter(|preview| preview.session == session)
        else {
            return;
        };
        let mut pending = self.library.transport_after(Duration::ZERO);
        preview._poll = Some(cx.spawn(async move |this, cx| loop {
            let snapshot = pending.await;
            let again = this
                .update(cx, |this, cx| {
                    let Some(preview) = this
                        .source_preview
                        .as_mut()
                        .filter(|preview| preview.session == session)
                    else {
                        return false;
                    };
                    let Ok(snapshot) = snapshot else {
                        return false;
                    };
                    if snapshot.session != session {
                        this.source_preview = None;
                        cx.notify();
                        return false;
                    }
                    if !preview.scrubbing {
                        preview.position = snapshot.current_time;
                        preview.playing = snapshot.is_playing;
                    }
                    cx.notify();
                    preview.playing || preview.scrubbing
                })
                .unwrap_or(false);
            if !again {
                return;
            }
            match this.read_with(cx, |this, _| this.library.transport_after(POLL)) {
                Ok(next) => pending = next,
                Err(_) => return,
            }
        }));
    }

    /// Move the playhead to `seconds`, keeping it playing if it was.
    pub(crate) fn seek_source_preview(&mut self, seconds: f32, cx: &mut Context<Self>) {
        let Some(preview) = self.source_preview.as_mut() else {
            return;
        };
        let Some(loaded) = &preview.loaded else {
            return;
        };
        let seconds = seconds.clamp(0.0, loaded.duration);
        preview.position = seconds;
        let session = preview.session;
        if preview.playing {
            let play = self.library.play(session, seconds);
            sent(play);
        } else {
            let seek = self.library.seek(session, seconds);
            sent(seek);
        }
        cx.notify();
    }

    /// Shift+←/→: `false` when there is no preview to move, so the key can
    /// fall through to the field.
    pub(crate) fn nudge_source_preview(&mut self, delta: f32, cx: &mut Context<Self>) -> bool {
        let Some(position) = self
            .source_preview
            .as_ref()
            .filter(|preview| preview.loaded.is_some())
            .map(|preview| preview.position)
        else {
            return false;
        };
        self.seek_source_preview(position + delta, cx);
        true
    }

    pub(crate) fn set_source_preview_scrubbing(&mut self, scrubbing: bool, cx: &mut Context<Self>) {
        let Some(preview) = self.source_preview.as_mut() else {
            return;
        };
        preview.scrubbing = scrubbing;
        let (session, playing) = (preview.session, preview.playing);
        // Letting go resumes following the transport.
        if !scrubbing && playing {
            self.poll_source_preview(session, cx);
        }
    }

    /// Drop the preview and free its audio, leaving the player empty.
    fn end_source_preview(&mut self) {
        if let Some(preview) = self.source_preview.take() {
            // Releasing stops it: the host unloads the session's audio.
            sent(self.library.release_audio(preview.session));
        }
    }

    /// Stop the preview and give the front score tab its audio back.
    pub(crate) fn stop_source_preview(&mut self, cx: &mut Context<Self>) {
        if self.source_preview.is_none() {
            return;
        }
        self.end_source_preview();
        self.sync_track_audio(cx);
        cx.notify();
    }
}

/// A transport control whose answer nothing waits on. Library commands start
/// on its runtime when they are called; the future only carries the reply,
/// and a control the host refuses (a session that is already gone) has
/// nothing to report to anyone.
fn sent<T>(command: impl std::future::Future<Output = T>) {
    drop(command);
}

/// Stop a preview whose dialog, route or source is gone — see the module
/// docs. Called by the shell every frame.
pub(crate) fn reap_preview(app: &mut Luma, cx: &mut Context<Luma>) {
    let Some(preview) = &app.source_preview else {
        return;
    };
    let alive = matches!(
        app.overlay.as_open(),
        Some(crate::shell::Overlay::AddTracks(state))
            if state.instance_id == preview.dialog
                && state.source_generation == preview.source_generation
                && *state.morph.target_key() == Route::SourceLibrary
    );
    if !alive {
        app.stop_source_preview(cx);
    }
}
