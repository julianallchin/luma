//! The strip above the timeline: the transport, add and export, readouts,
//! and the beat-grid vote.

use super::*;

/// Room the toolbar keeps for its edit readouts: a selection count and a
/// cursor range.
const EDIT_READOUTS_WIDTH: f32 = 190.;

/// What is open, what can be added to it, and whether a write is in the air.
pub(super) fn toolbar(state: &Editor, app: &Entity<Luma>, readout: Entity<Readout>) -> Div {
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
        .child(readout)
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(state.track_name.clone())
                .agent_node(Role::Text, state.track_name.clone()),
        )
        .child(
            luma_ui::button("Add pattern", Enabled::Yes)
                .id("add-score-pattern")
                .on_click(move |_, _, cx| insert.update(cx, |app, cx| app.add_pattern(cx)))
                .agent_node(Role::Button, "Add pattern"),
        )
        .child(
            luma_ui::button("Export show", state.exportable().into())
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
}

/// The transport at the strip's head: play or pause, where the song is
/// against how long it runs, and its tempo.
///
/// A view of its own, because the clock has to run with the playhead and
/// playback never notifies the application root: it repaints whenever the
/// timeline's surface does, which is every frame the song plays and every
/// scrub.
pub(super) struct Readout {
    app: WeakEntity<Luma>,
    _surface: Subscription,
}

impl Readout {
    pub(super) fn new(
        app: WeakEntity<Luma>,
        surface: &Entity<playback_surface::Surface>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            app,
            _surface: cx.observe(surface, |_, _, cx| cx.notify()),
        }
    }
}

impl Render for Readout {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let Some(Body::TrackEditor(state)) = app.read(cx).workspace.active_body() else {
            return div().into_any_element();
        };
        let playing = state.transport.playing;
        let ready = state.transport.ready && state.transport.session.is_some();
        let label = if playing { "Pause" } else { "Play" };
        let position = format!(
            "{} / {}",
            clock(state.transport.position),
            clock(state.transport.duration)
        );
        let transport = app.clone();
        div()
            .flex()
            .items_center()
            .gap(px(8.))
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
                // One button, two labels — the label *is* the state, so a
                // script reads what the transport is doing from the same place
                // a person does.
                .on_click(move |_, _, cx| transport.update(cx, |this, cx| this.toggle_playback(cx)))
                .agent_node(Role::Button, label),
            )
            .child(luma_ui::caption(position))
            .children(
                state
                    .beats
                    .as_deref()
                    .map(|grid| luma_ui::caption(format!("{:.1} BPM", grid.bpm))),
            )
            .agent_node(Role::Card, "Transport")
            .into_any_element()
    }
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
        });
        cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |this, cx| {
                let saved = result.is_ok();
                this.edit_track_tab(&target, cx, |editor| {
                    editor.beat_validation_pending = false;
                    if saved {
                        editor.beat_verdict = verdict;
                        editor.beat_reason = reason;
                    }
                });
                if let Err(error) = result {
                    this.report_failure("Could not save the beat check", error, cx);
                }
            })
            .ok();
        })
        .detach();
    }
}
