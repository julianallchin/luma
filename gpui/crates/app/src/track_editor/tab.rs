//! The editor as a workspace tab: opening a score, loading its contents, and
//! reaching its editor from async work later. These hang off `Luma` because
//! opening a track is five `Library` calls plus a screen transition, and `Luma`
//! owns both.

use super::*;

/// A track's title, or its file name when it has no title.
fn track_title(track: &TrackBrowserRow) -> String {
    if let Some(title) = track.title.as_ref().filter(|title| !title.is_empty()) {
        return title.clone();
    }
    std::path::Path::new(&track.file_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| track.file_path.clone())
}

impl Luma {
    /// Open `score` of `track_id` as a tab in the selected venue, or bring its
    /// tab to the front: the tab's identity is its score. `thread` is the chat
    /// a restored tab had open.
    ///
    /// The four reads are started together and awaited in order: they do not
    /// depend on each other, and each is already its own task on the Tokio
    /// runtime, so sequencing the `await`s costs nothing and keeps the
    /// assignment in one place.
    pub(crate) fn open_score(
        &mut self,
        track_id: &str,
        score: Score,
        thread: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(browser) = &self.songs else {
            return;
        };
        let Some(track) = browser.find(track_id) else {
            return;
        };
        let venue_id = browser.venue_id().to_string();
        let target = Target::Score {
            venue: venue_id.clone(),
            track: track.id.clone(),
            score: score.id.clone(),
        };
        if self.workspace.body(&target).is_some() {
            self.workspace.set_mode(target.mode());
            cx.notify();
            return;
        }

        let waveform = self.library.track_waveform(track_id);
        let beats = self.library.track_beats(track_id);
        let validation = self.library.track_beat_validation(track_id);
        let contents = self.library.score_contents(&score.id, track_id);

        let menu_search =
            cx.new(|cx| luma_ui::text_input::TextInput::search("Search patterns…", cx));
        let search_target = target.clone();
        let menu_subscription = cx.subscribe(&menu_search, move |this, field, event, cx| {
            if event == &luma_ui::text_input::Event::Edited {
                let query = field.read(cx).text().to_string();
                this.edit_track_tab(&search_target, cx, |editor| {
                    if editor.menu_query == query {
                        return;
                    }
                    editor.menu_query = query;
                    if let Some(menu) = editor.menu.as_mut() {
                        menu.active = 0;
                    }
                    editor.menu_scroll.scroll_to_item(0);
                });
            }
        });
        let chat = self.body_chat(&target, thread, cx);
        let state = Box::new(Editor {
            track_id: track.id.clone(),
            track_name: track_title(&track),
            venue_id: venue_id.clone(),
            score,
            chat,
            waveform: None,
            gpu_waveform: Rc::new(RefCell::new(waveform::Resource::default())),
            timeline_waveform: waveform::Strip::default(),
            overview_waveform: waveform::Strip::default(),
            minimap_drag: None,
            beats: None,
            beat_verdict: BeatValidationVerdict::Unreviewed,
            beat_reason: None,
            beat_reason_menu: Default::default(),
            beat_validation_pending: false,
            beat_validation_error: None,
            clips: Vec::new().into(),
            graph_score: None,
            previews: Rc::new(RefCell::new(HashMap::new())),
            preview_inflight: HashSet::new(),
            preview_errors: HashMap::new(),
            preview_queued: HashSet::new(),
            history: History::default(),
            clipboard: None,
            loop_region: None,
            menu: None,
            menu_query: String::new(),
            menu_search,
            _menu_subscription: menu_subscription,
            menu_scroll: ScrollHandle::new(),
            selected: Vec::new(),
            graph_open: false,
            pressed_clip: None,
            cursor: None,
            follow: false,
            view: View {
                zoom: View::DEFAULT_ZOOM,
                scroll: 0.,
                zoom_y: 1.,
                lift: 0.,
            },
            transport: Transport::default(),
            playback_surface: None,
            transport_readout: None,
            gesture: None,
            alpha_hover: None,
            canvas: Rc::new(Cell::new(Bounds::default())),
            anchor: None,
            zoom_motion: None,
            seek_pending: None,
            seek_at: None,
            dirty: false,
            saving: false,
            writes: 0,
            error: None,
            loaded: false,
            composited: None,
            compositing: false,
            sheet: sheet::State::default(),
        });
        self.open_body(target.clone(), move || Body::TrackEditor(state), cx);

        cx.spawn(async move |this, cx| {
            let waveform = waveform.await;
            let beats = beats.await;
            let validation = validation.await;
            let contents = match contents.await {
                Ok(contents) => Ok(cx
                    .background_spawn(async move { document::Resolved::of(contents) })
                    .await),
                Err(error) => Err(error),
            };

            this.update(cx, |this, cx| {
                // Addressed to the tab the load was started for, not to
                // whichever tab is visible when it lands.
                this.edit_track_tab(&target, cx, |editor| {
                    editor.loaded = true;
                    match waveform {
                        Ok(waveform) => {
                            editor.transport.duration = waveform.duration_seconds as f32;
                            editor.waveform = Some(Rc::new(waveform));
                        }
                        Err(error) => editor.error = Some(error.to_string()),
                    }
                    editor.beats = beats.ok().flatten().map(Rc::new);
                    match validation {
                        Ok(grid) => {
                            if let Some(validation) = grid.filter(|validation| {
                                Some(&validation.grid) == editor.beats.as_deref()
                            }) {
                                editor.beat_verdict = validation.verdict;
                                editor.beat_reason = validation.reason;
                            }
                        }
                        Err(error) => editor.beat_validation_error = Some(error.to_string()),
                    }
                });
                this.install_score(target, contents, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Put a score's stored contents on its tab's timeline, with their
    /// previews and the stage's scene.
    fn install_score(
        &mut self,
        target: Target,
        contents: Result<document::Resolved, LibraryError>,
        cx: &mut Context<Self>,
    ) {
        let mut previews = Vec::new();
        let mut split = false;
        self.edit_track_tab(&target, cx, |editor| match contents {
            Ok(resolved) => match editor.install_contents(resolved) {
                Ok(()) => {
                    previews = editor.clips.iter().map(|clip| clip.id.clone()).collect();
                    split = editor.dirty;
                }
                Err(error) => editor.error = Some(error),
            },
            Err(error) => editor.error = Some(error.to_string()),
        });
        self.refresh_clip_previews_for(target.clone(), previews, cx);
        self.refresh_working_scene_for(&target, cx);
        if split {
            self.commit_graph_score_for(target, cx);
        }
    }

    /// Run `edit` against one editor tab, wherever it sits in the strip. The
    /// async loads come through here so a waveform landing late cannot write
    /// into whichever tab happens to be visible.
    pub(super) fn edit_track_tab(
        &mut self,
        target: &Target,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Editor),
    ) {
        if let Some(Body::TrackEditor(editor)) = self.venues.body_mut(&mut self.workspace, target) {
            edit(editor);
            cx.notify();
        }
    }

    pub(super) fn edit_waveform_tab(
        &mut self,
        target: &Target,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Editor),
    ) {
        if let Some(Body::TrackEditor(editor)) = self.workspace.body_mut(target) {
            edit(editor);
            // Playback already schedules the next display frame. Publishing a
            // texture must not insert an extra, off-cadence animation step.
            if editor.transport.playing && editor.timeline_waveform.frame.is_some() {
                return;
            }
            if let Some(surface) = editor.playback_surface.clone() {
                surface.update(cx, |_, cx| cx.notify());
            } else {
                cx.notify();
            }
        }
    }

    /// Run `edit` against the track editor, if that is still what is showing.
    /// A load or a write that lands after the user navigated away is a no-op.
    pub(super) fn with_track_editor(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Editor),
    ) {
        if let Some(Body::TrackEditor(state)) = self.workspace.active_body_mut() {
            edit(state);
            cx.notify();
        }
    }
}
