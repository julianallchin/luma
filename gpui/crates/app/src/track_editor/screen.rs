//! The screen's entry points: the tab body, Direct's clip graph page, the
//! inspector, and the fullscreen transport.

use super::*;

/// The inspector occupies the editing area, even with the rig hidden: the
/// selected clip's controls, or the preset browser — see [`sheet::panel`].
pub(crate) fn inspector(
    state: &mut Editor,
    app: &Entity<Luma>,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> AnyElement {
    sheet::sync(sheet::HostMut::Track(state), window, cx);
    sheet::panel(sheet::Host::Track(state), app, None)
}

/// Direct's graph page: the selected clip's inspector where the timeline was,
/// with a way back to it at the head of the inspector's strip.
///
/// Rendered inside the tab, in place of [`track_editor`], so the page keeps
/// the tab's focus — ⌘Z still undoes, and Escape reaches the ladder that
/// closes the page — under the clip graph's key context rather than the
/// timeline's (see `shell::active_tab`). And it keeps the working score
/// composited for the same reason [`fullscreen_transport`] does: the stage
/// above is lit by edits made here.
pub(crate) fn clip_graph_page(
    state: &mut Editor,
    app: &Entity<Luma>,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> AnyElement {
    sync_composite(state, cx);
    let back = sheet::back(
        "clip-graph-back",
        "Timeline",
        "Back to timeline",
        app,
        |this, cx| {
            this.close_clip_graph(cx);
        },
    );
    sheet::sync(sheet::HostMut::Track(state), window, cx);
    div()
        .size_full()
        .flex()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(sheet::panel(sheet::Host::Track(state), app, Some(back)))
        .into_any_element()
}

/// Render the screen: a toolbar strip over the canvas.
///
/// The same split as the graph editor, and for the same reason — a panel is a
/// stack of boxes and gpui lays boxes out well, while a timeline is a
/// coordinate system nothing gpui lays out could express without a box per
/// clip per beat.
pub fn track_editor(
    state: &mut Editor,
    app: &Entity<Luma>,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> Div {
    let surface = state
        .playback_surface
        .get_or_insert_with(|| cx.new(|_| playback_surface::Surface::new(app.downgrade())))
        .clone();
    if state
        .beat_reason_menu
        .tick_close(luma_ui::motion::reduced_motion(cx))
    {
        window.request_animation_frame();
    }

    // …and the rig against the working copy, for the same reason: an edit is
    // only real once the scene it changed has been installed.
    sync_composite(state, cx);
    let state = &*state;
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(toolbar(state, app))
        // An error only takes the screen when there is nothing behind it to
        // show. A *write* that came back refused — an overlap the seam
        // forbids, a document it would not validate — leaves a perfectly
        // good timeline on screen, and
        // replacing it with a sentence would throw away the picture the user
        // needs in order to understand the refusal. Those read out on the
        // toolbar instead.
        .child(match (&state.error, state.waveform.is_some()) {
            (Some(message), false) => luma_ui::plate(message.clone(), ladder::danger()),
            (None, false) => {
                luma_ui::plate("Loading track…".to_string(), ladder::muted_foreground())
            }
            (_, true) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .relative()
                .overflow_hidden()
                .child(surface)
                .into_any_element(),
        })
}

/// Keep the working score and its transport live while the timeline is hidden.
pub(crate) fn fullscreen_transport(
    state: &mut Editor,
    app: &Entity<Luma>,
    cx: &mut Context<Luma>,
) -> AnyElement {
    sync_composite(state, cx);
    let transport = app.clone();
    let position = state
        .transport
        .clock
        .position(std::time::Instant::now())
        .unwrap_or(f64::from(state.transport.position))
        .min(f64::from(state.transport.duration)) as f32;
    let label = if state.transport.playing {
        "Pause"
    } else {
        "Play"
    };
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .child(
            luma_ui::button(
                label,
                (state.waveform.is_some()
                    && state.transport.ready
                    && state.transport.session.is_some())
                .into(),
            )
            .id("fullscreen-transport")
            .on_click(move |_, _, cx| transport.update(cx, |this, cx| this.toggle_playback(cx)))
            .agent_node(Role::Button, label),
        )
        .child(
            div()
                .text_size(px(12.))
                .pr(px(8.))
                .child(format!(
                    "{} / {}",
                    clock(position),
                    clock(state.transport.duration)
                ))
                .agent_node(
                    Role::Text,
                    format!("{} / {}", clock(position), clock(state.transport.duration)),
                ),
        )
        .into_any_element()
}

impl Luma {
    /// Put the selected clip's graph where the timeline is. Direct only:
    /// Program's inspector already shows it beside the timeline, and a flag
    /// set there would spring the page open on the next switch to Direct.
    pub(crate) fn open_clip_graph(&mut self, cx: &mut Context<Self>) {
        if self.view_mode != ViewMode::Direct {
            return;
        }
        self.with_track_editor(cx, |editor| {
            editor.graph_open = !editor.selected.is_empty();
        });
    }

    /// Back from the graph page to the timeline, keeping the selection.
    /// Reports whether there was a page to leave, for the Escape ladder.
    pub(crate) fn close_clip_graph(&mut self, cx: &mut Context<Self>) -> bool {
        let direct = self.view_mode == ViewMode::Direct;
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body_mut() else {
            return false;
        };
        if !direct || !editor.clip_graph_shown() {
            return false;
        }
        editor.graph_open = false;
        cx.notify();
        true
    }

    /// Close whichever menu the args sheet has up, if it has one. `Escape`'s
    /// first business once nothing is floating over the screen.
    pub(crate) fn dismiss_sheet_menu(&mut self) -> bool {
        match self.workspace.active_body_mut() {
            Some(Body::TrackEditor(state)) => {
                if state.beat_reason_menu.is_open() {
                    state.beat_reason_menu.close();
                    true
                } else {
                    state.sheet.dismiss_menu()
                }
            }
            Some(Body::Perform(state)) => state
                .pattern_sheet
                .as_mut()
                .is_some_and(|open| open.sheet.dismiss_menu()),
            _ => false,
        }
    }
}
