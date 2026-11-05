//! Playback on the audio host: loading a track or file as a session, the
//! transport verbs, and synchronous reads of where the transport is.

use std::future::Future;
use std::time::Duration;

use luma_lib::dispatch::handlers;
use luma_lib::dispatch::handlers::host_audio::FilePreview;
use luma_lib::host_audio::HostAudioSnapshot;

use super::{refused, Library, LibraryError};

impl Library {
    /// Decode a track and hand it to the audio host, from 0.0. Slow — this is
    /// the decode — and it must land before the transport does anything.
    pub fn load_audio(
        &self,
        track_id: &str,
    ) -> (u64, impl Future<Output = Result<(), LibraryError>> + use<>) {
        let session = next_audio_session();
        let track_id = track_id.to_owned();
        (
            session,
            self.call("host_load_track", move |services| async move {
                handlers::host_audio::host_load_track(&services, track_id, session).await
            }),
        )
    }

    /// Put an audio file that is not a library track on the player — a DJ
    /// library row being auditioned — as a new session. The same player and
    /// the same session order as [`Self::load_audio`]: whichever was asked
    /// for last owns the speakers. `Ok(None)` when a newer session took the
    /// player first.
    pub fn load_file_audio(
        &self,
        path: &str,
    ) -> (
        u64,
        impl Future<Output = Result<Option<FilePreview>, LibraryError>> + use<>,
    ) {
        let session = next_audio_session();
        let path = path.to_owned();
        (
            session,
            self.call("host_load_file", move |services| async move {
                handlers::host_audio::host_load_file(&services, path, session).await
            }),
        )
    }

    /// Free `session`'s decoded audio, if it still owns the player.
    pub fn release_audio(
        &self,
        session: u64,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("host_release", move |services| async move {
            handlers::host_audio::host_release(&services, session).await
        })
    }

    /// Seeking and playing are atomic at the audio host.
    pub fn play(
        &self,
        session: u64,
        seconds: f32,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("host_play", move |services| async move {
            handlers::host_audio::host_play(&services, session, seconds).await
        })
    }

    pub fn pause(&self, session: u64) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("host_pause", move |services| async move {
            handlers::host_audio::host_pause(&services, session).await
        })
    }

    pub fn seek(
        &self,
        session: u64,
        seconds: f32,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("host_seek", move |services| async move {
            handlers::host_audio::host_seek(&services, session, seconds).await
        })
    }

    pub fn set_loop_region(
        &self,
        session: u64,
        region: Option<(f32, f32)>,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.call("host_set_loop_region", move |services| async move {
            handlers::host_audio::host_set_loop_region(
                &services,
                session,
                region.map(|(start, _)| start),
                region.map(|(_, end)| end),
            )
            .await
        })
    }

    /// Restore a song's transport before enabling its controls.
    pub fn restore_audio(
        &self,
        session: u64,
        seconds: f32,
        region: Option<(f32, f32)>,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.task("host_seek", move |services| async move {
            handlers::host_audio::host_seek(&services, session, seconds)
                .await
                .map_err(refused("host_seek"))?;
            handlers::host_audio::host_set_loop_region(
                &services,
                session,
                region.map(|r| r.0),
                region.map(|r| r.1),
            )
            .await
            .map_err(refused("host_set_loop_region"))
        })
    }

    /// The transport, `after` a wait.
    ///
    /// Native playback polls on the Tokio runtime because that is the one
    /// this process has real timers on. GPUI's own timers are
    /// driven by a test clock under the harness, so a poll paced there would
    /// never tick in a test.
    pub fn transport_after(
        &self,
        after: Duration,
    ) -> impl Future<Output = Result<HostAudioSnapshot, LibraryError>> + use<> {
        self.call_after("host_snapshot", after, |services| async move {
            handlers::host_audio::host_snapshot(&services).await
        })
    }

    /// Where the transport is, right now, in track seconds. Synchronous for
    /// the same reason as [`Self::sample_universe`].
    pub fn render_time(&self) -> f32 {
        self.services.host_audio().render_time()
    }

    /// What the transport is doing, right now — whether it is running, and how
    /// long what it is running is.
    ///
    /// Separate from [`Self::render_time`] because they answer different
    /// questions: that one is the absolute instant a frame is *of*, this one is
    /// the state a transport control is a picture of. The snapshot's own
    /// `current_time` is relative to the loaded segment and is deliberately not
    /// used as a frame's `t`.
    pub fn transport(&self) -> HostAudioSnapshot {
        self.services.host_audio().snapshot()
    }

    /// The decoded audio of `track_id`, when it is the track the player has
    /// loaded — shared with the player, not copied.
    pub fn track_audio(&self, track_id: &str) -> Option<luma_lib::host_audio::TrackAudio> {
        let (loaded, audio) = self.services.host_audio().audio()?;
        (loaded == track_id).then_some(audio)
    }
}

/// The next playback session. One counter for every kind of load, allocated
/// before spawning: the host keeps whichever session is newest, so task
/// scheduling must not be able to reorder what the person asked for.
fn next_audio_session() -> u64 {
    static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
