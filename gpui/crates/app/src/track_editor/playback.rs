use super::*;

impl Editor {
    pub(crate) fn playback_session(&self) -> Option<u64> {
        self.transport.session
    }
}

impl Luma {
    /// Leaving a song preserves its playhead and loop, but releases playback.
    pub(crate) fn park_track_audio(&mut self, cx: &mut Context<Self>) {
        let targets: Vec<_> = self
            .workspace
            .iter()
            .map(|tab| tab.target.clone())
            .collect();
        for target in targets {
            let Some(Body::TrackEditor(editor)) = self.workspace.body_mut(&target) else {
                continue;
            };
            editor.transport.polling = None;
            editor.transport.playing = false;
            editor.transport.ready = false;
            editor.transport.clock.reset();
            if let Some(session) = editor.transport.session.take() {
                let pause = self.library.pause(session);
                cx.background_spawn(async move {
                    pause.await.ok();
                })
                .detach();
            }
        }
    }

    pub(crate) fn activate_track_audio(&mut self, target: &Target, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.body_mut(target) else {
            return;
        };
        if editor.transport.session.is_some() {
            return;
        }
        let (session, pending) = self.library.load_audio(&editor.track_id);
        editor.transport.session = Some(session);
        editor.transport.ready = false;
        editor.transport.playing = false;
        editor.transport.polling = None;
        editor.transport.clock.reset();
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            let setup = this.update(cx, |this, cx| {
                let Some(Body::TrackEditor(editor)) = this.workspace.body_mut(&target) else {
                    return None;
                };
                if editor.transport.session != Some(session) {
                    return None;
                }
                if let Err(error) = result {
                    editor.transport.ready = false;
                    editor.error = Some(error.to_string());
                    cx.notify();
                    return None;
                }
                Some(this.library.restore_audio(
                    session,
                    editor.transport.position,
                    editor.loop_region.map(|(a, b)| (a as f32, b as f32)),
                ))
            });
            let Ok(Some(setup)) = setup else {
                return;
            };
            let result = setup.await;
            this.update(cx, |this, cx| {
                let Some(Body::TrackEditor(editor)) = this.workspace.body_mut(&target) else {
                    return;
                };
                if editor.transport.session != Some(session) {
                    return;
                }
                editor.transport.ready = result.is_ok();
                if let Err(error) = result {
                    editor.error = Some(error.to_string());
                }
                this.poll_transport_for(&target, session, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body() else {
            return;
        };
        self.set_track_playback(!editor.transport.playing, editor.transport.position, cx);
    }

    pub(crate) fn play_track_at(&mut self, seconds: f32, cx: &mut Context<Self>) {
        self.set_track_playback(true, seconds, cx);
    }

    fn set_track_playback(&mut self, playing: bool, seconds: f32, cx: &mut Context<Self>) {
        let Some(target) = self.workspace.active().cloned() else {
            return;
        };
        let Some(Body::TrackEditor(editor)) = self.workspace.body_mut(&target) else {
            return;
        };
        let Some(session) = editor.transport.session.filter(|_| editor.transport.ready) else {
            return;
        };
        let pending: Transition = if playing {
            Box::pin(self.library.play(session, seconds))
        } else {
            Box::pin(self.library.pause(session))
        };
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let Some(Body::TrackEditor(editor)) = this.workspace.body_mut(&target) else {
                    return;
                };
                if editor.transport.session != Some(session) {
                    return;
                }
                match result {
                    Ok(()) => this.poll_transport_for(&target, session, cx),
                    Err(error) => editor.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn poll_transport_for(&mut self, target: &Target, session: u64, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.body_mut(target) else {
            return;
        };
        let target = target.clone();
        let mut pending = self.library.transport_after(Duration::ZERO);
        editor.transport.polling = Some(cx.spawn(async move |this, cx| loop {
            let snapshot = pending.await;
            let again = this
                .update(cx, |this, cx| {
                    this.apply_transport(&target, session, snapshot, cx)
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

    fn apply_transport(
        &mut self,
        target: &Target,
        session: u64,
        snapshot: Result<HostAudioSnapshot, LibraryError>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(Body::TrackEditor(state)) = self.workspace.body_mut(target) else {
            return false;
        };
        let Ok(snapshot) = snapshot else {
            return false;
        };
        if state.transport.session != Some(session)
            || snapshot.session != session
            || snapshot.track_id.as_deref() != Some(state.track_id.as_ref())
        {
            return false;
        }
        let status_second = snapshot.current_time.max(0.) as u64;
        let notify_status = state.transport.playing != snapshot.is_playing
            || state.transport.status_second != status_second
            || state.transport.duration != snapshot.duration_seconds;
        state.transport.status_second = status_second;
        state.transport.playing = snapshot.is_playing;
        if !matches!(state.gesture, Some(Gesture::Scrub)) {
            let now = std::time::Instant::now();
            state.transport.clock.observe(
                now,
                f64::from(snapshot.current_time),
                snapshot.is_playing,
            );
            state.transport.position = state
                .transport
                .clock
                .position(now)
                .unwrap_or(f64::from(snapshot.current_time))
                as f32;
        }
        if snapshot.is_loaded && snapshot.duration_seconds > 0. {
            state.transport.duration = snapshot.duration_seconds;
        }
        state.follow_playhead();
        if notify_status || !snapshot.is_playing {
            cx.notify();
        }
        snapshot.is_playing
    }
}
