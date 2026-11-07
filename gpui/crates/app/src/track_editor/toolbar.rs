//! The strip above the timeline — add and export, readouts, and the beat-grid
//! vote — and the audio lane's header, which holds the transport.

use super::*;

/// Room the toolbar keeps for its edit readouts: a selection count and a
/// cursor range.
const EDIT_READOUTS_WIDTH: f32 = 190.;

/// What is open, what can be added to it, and whether a write is in the air.
pub(super) fn toolbar(state: &Editor, app: &Entity<Luma>) -> Div {
    let insert = app.clone();
    let export = app.clone();
    div()
        .flex()
        .flex_wrap()
        .flex_shrink_0()
        .items_center()
        .gap(px(12.))
        .px(px(16.))
        .py(px(8.))
        .border_b_1()
        .border_color(ladder::trim())
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(state.track_name.clone())
                .agent_node(Role::Text, state.track_name.clone()),
        )
        .child(
            luma_ui::button(
                "Add pattern",
                if state.writable() {
                    Enabled::Yes
                } else {
                    Enabled::No
                },
            )
            .id("add-score-pattern")
            .on_click(move |_, _, cx| insert.update(cx, |app, cx| app.add_pattern(cx)))
            .agent_node(Role::Button, "Add pattern"),
        )
        .child(
            luma_ui::button("Export show", state.export_source().is_some().into())
                .id("export-show")
                .on_click(move |_, _, cx| export.update(cx, |app, cx| app.open_show_export(cx)))
                .agent_node(Role::Button, "Export show"),
        )
        .child(luma_ui::caption(match state.clips.len() {
            1 => "1 pattern".to_string(),
            count => format!("{count} patterns"),
        }))
        .when(
            state
                .beats
                .as_ref()
                .is_some_and(|grid| !grid.beats.is_empty() && !grid.downbeats.is_empty()),
            |el| {
                let enabled = if state.beat_validation_pending {
                    Enabled::No
                } else {
                    Enabled::Yes
                };
                let votes = div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().text_size(px(12.)).child("Beat grid"))
                    .children(
                        [
                            (
                                BeatValidationVerdict::Correct,
                                luma_ui::icons::IconName::ThumbsUp,
                                if state.beat_verdict == BeatValidationVerdict::Correct {
                                    "Beat grid approved"
                                } else {
                                    "Beat grid correct"
                                },
                            ),
                            (
                                BeatValidationVerdict::Incorrect,
                                luma_ui::icons::IconName::ThumbsDown,
                                if state.beat_verdict == BeatValidationVerdict::Incorrect {
                                    "Beat grid flagged"
                                } else {
                                    "Needs correction"
                                },
                            ),
                        ]
                        .into_iter()
                        .map(|(verdict, icon, label)| {
                            let selected = state.beat_verdict == verdict;
                            let next = if selected {
                                BeatValidationVerdict::Unreviewed
                            } else {
                                verdict
                            };
                            let app = app.clone();
                            luma_ui::button("", enabled)
                                .w(px(28.))
                                .px(px(0.))
                                .id(label)
                                .child(gpui_component::Icon::new(icon).size(px(14.)))
                                .when(selected, |el| el.text_color(ladder::primary()))
                                .on_click(move |_, _, cx| {
                                    app.update(cx, |this, cx| {
                                        this.save_beat_validation(next, None, cx)
                                    })
                                })
                                .agent_node(Role::Button, label)
                        }),
                    );
                el.child(votes.when(
                    state.beat_verdict == BeatValidationVerdict::Incorrect,
                    |el| {
                        let value = match state.beat_reason {
                            None => "Reason (optional)",
                            Some(BeatValidationReason::Tempo) => "Wrong tempo",
                            Some(BeatValidationReason::Offset) => "Offset",
                            Some(BeatValidationReason::Drift) => "Drift",
                            Some(BeatValidationReason::BarPhase) => "Bar phase",
                        };
                        let toggle = app.clone();
                        let pick = app.clone();
                        el.child(luma_ui::arg::select::luma_arg_select(
                            "beat-grid-reason",
                            value,
                            &[
                                "Reason (optional)",
                                "Wrong tempo",
                                "Offset",
                                "Drift",
                                "Bar phase",
                            ],
                            state.beat_reason_menu,
                            move |_, cx| {
                                toggle.update(cx, |this, cx| {
                                    this.with_track_editor(cx, |editor| {
                                        if !editor.beat_validation_pending {
                                            editor.beat_reason_menu.toggle();
                                        }
                                    });
                                })
                            },
                            move |index, _, cx| {
                                pick.update(cx, |this, cx| {
                                    let reason = [
                                        None,
                                        Some(BeatValidationReason::Tempo),
                                        Some(BeatValidationReason::Offset),
                                        Some(BeatValidationReason::Drift),
                                        Some(BeatValidationReason::BarPhase),
                                    ][index];
                                    this.save_beat_validation(
                                        BeatValidationVerdict::Incorrect,
                                        reason,
                                        cx,
                                    );
                                })
                            },
                        ))
                    },
                ))
            },
        )
        .when_some(state.beat_validation_error.clone(), |el, message| {
            el.child(
                luma_ui::float::label(message.clone())
                    .text_color(ladder::danger())
                    .agent_node(Role::Text, message),
            )
        })
        // What the edit is on: the selection, the cursor, the loop, follow.
        // One slot, its width held whether or not anything is selected, so
        // selecting a pattern never reflows the toolbar and moves the
        // timeline under the pointer.
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .min_w(px(EDIT_READOUTS_WIDTH))
                .when(!state.selected.is_empty(), |el| {
                    el.child(luma_ui::caption(format!(
                        "{} selected",
                        state.selected.len()
                    )))
                })
                // The cursor's own readout: a point reads as one time, a range
                // as the span it covers. It is the only text that says where an
                // edit would land.
                .when_some(state.cursor, |el, cursor| {
                    el.child(luma_ui::caption(match cursor.span() {
                        Some((from, to)) => format!("Cursor {from:.2}-{to:.2}"),
                        None => format!("Cursor {:.2}", cursor.start),
                    }))
                })
                .when_some(state.loop_region, |el, (from, to)| {
                    el.child(luma_ui::caption(format!("Loop {from:.2}-{to:.2}")))
                })
                .when(state.follow, |el| el.child(luma_ui::caption("Follow"))),
        )
        .child(div().flex_1())
        // Which score is on the timeline, by the handle the song list names it
        // by.
        .child(luma_ui::caption(format!("Score #{}", state.score.ordinal)))
        // A refused write, over the timeline it was refused for.
        .when_some(
            state
                .error
                .clone()
                .or_else(|| state.gpu_waveform.borrow().error.clone())
                .or_else(|| state.timeline_waveform.error.clone())
                .or_else(|| state.overview_waveform.error.clone())
                .or_else(|| state.preview_errors.values().next().cloned())
                .filter(|_| state.waveform.is_some()),
            |el, message| {
                el.child(
                    luma_ui::float::label(message.clone())
                        .text_color(ladder::danger())
                        .agent_node(Role::Text, message),
                )
            },
        )
        .when(state.score.read_only, |el| {
            el.child(luma_ui::caption("Read only"))
        })
}

/// Where the audio lane's header sits: its left edge, inset from the
/// timeline's.
const AUDIO_HEADER_INSET: f32 = 8.;

/// The audio lane's header: play, where the song is, and its tempo, over the
/// waveform's leading edge.
///
/// A panel of its own rather than a strip above the timeline, so the
/// transport sits with the lane it plays. It takes the presses that land on
/// it and no others — the waveform around it still clears the selection.
pub(super) fn audio_header(state: &Editor, app: &Entity<Luma>) -> impl IntoElement {
    let transport = app.clone();
    let playing = state.transport.playing;
    let label = if playing { "Pause" } else { "Play" };
    let ready = state.transport.ready && state.transport.session.is_some();
    let position = format!(
        "{} / {}",
        clock(state.transport.position),
        clock(state.transport.duration)
    );
    div()
        .absolute()
        .left(px(AUDIO_HEADER_INSET))
        .top(px(HEADER_HEIGHT))
        .h(px(WAVEFORM_HEIGHT))
        .flex()
        .items_center()
        .child(
            div()
                .id("audio-lane-header")
                .occlude()
                .flex()
                .flex_col()
                .gap(px(2.))
                .p(px(4.))
                .rounded(px(luma_ui::radius::CONTROL))
                .bg(luma_ui::glass::panel_opaque())
                .border_1()
                .border_color(luma_ui::glass::inner_rule())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            luma_ui::icon_button(
                                if playing {
                                    luma_ui::icons::IconName::Pause
                                } else {
                                    luma_ui::icons::IconName::Play
                                },
                                ready.into(),
                            )
                            .id("transport")
                            // One button, two labels — the label *is* the
                            // state, so a script reads what the transport is
                            // doing from the same place a person does.
                            .on_click(move |_, _, cx| {
                                transport.update(cx, |this, cx| this.toggle_playback(cx))
                            })
                            .agent_node(Role::Button, label),
                        )
                        .child(luma_ui::caption(position)),
                )
                .when_some(state.beats.as_deref(), |el, grid| {
                    el.child(
                        div()
                            .px(px(4.))
                            .child(luma_ui::caption(format!("{:.1} BPM", grid.bpm))),
                    )
                })
                .agent_node(Role::Card, "Audio lane header"),
        )
}

/// `M:SS`, the same clock the browser's TIME column reads in.
pub(crate) fn clock(seconds: f32) -> String {
    let total = if seconds.is_finite() {
        seconds.max(0.)
    } else {
        0.
    };
    format!("{}:{:02}", (total / 60.) as u64, (total % 60.) as u64)
}

impl Luma {
    fn save_beat_validation(
        &mut self,
        verdict: BeatValidationVerdict,
        reason: Option<BeatValidationReason>,
        cx: &mut Context<Self>,
    ) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body() else {
            return;
        };
        if editor.beat_validation_pending {
            return;
        }
        let Some(grid) = editor.beats.as_deref() else {
            return;
        };
        let target = editor.target();
        let request =
            self.library
                .set_track_beat_validation(&editor.track_id, grid, verdict, reason);
        self.edit_track_tab(&target, cx, |editor| {
            editor.beat_reason_menu.close();
            editor.beat_validation_pending = true;
            editor.beat_validation_error = None;
        });
        cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |this, cx| {
                this.edit_track_tab(&target, cx, |editor| {
                    editor.beat_validation_pending = false;
                    match result {
                        Ok(()) => {
                            editor.beat_verdict = verdict;
                            editor.beat_reason = reason;
                        }
                        Err(error) => editor.beat_validation_error = Some(error.to_string()),
                    }
                });
            })
            .ok();
        })
        .detach();
    }
}
