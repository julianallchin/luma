//! Whole-track navigation, using the timeline's waveform painter.
use super::*;

const HEIGHT: f32 = 40.;
const HANDLE: f32 = 6.;

#[derive(Clone, Copy)]
pub(super) enum Drag {
    Pan { offset: f64 },
    Start { end: f64, offset: f64 },
    End { start: f64, offset: f64 },
}

impl Editor {
    fn minimap_time(&self, at: Point<Pixels>) -> f64 {
        let bounds = self.overview_waveform.bounds.get();
        f64::from(f32::from(at.x - bounds.origin.x) / f32::from(bounds.size.width).max(1.))
            * f64::from(self.transport.duration)
    }

    fn minimap_press(&mut self, at: Point<Pixels>) {
        self.zoom_motion = None;
        let duration = f64::from(self.transport.duration);
        if duration <= 0. {
            return;
        }
        let time = self.minimap_time(at).clamp(0., duration);
        let (start, end) = self.visible();
        let end = end.min(duration);
        let handle = (duration
            * f64::from(
                HANDLE / f32::from(self.overview_waveform.bounds.get().size.width).max(1.),
            ))
        .min((end - start) / 3.);
        self.minimap_drag = Some(if (time - start).abs().min((time - end).abs()) <= handle {
            if (time - start).abs() < (time - end).abs() {
                Drag::Start {
                    end,
                    offset: time - start,
                }
            } else {
                Drag::End {
                    start,
                    offset: time - end,
                }
            }
        } else if time >= start && time <= end {
            Drag::Pan {
                offset: time - start,
            }
        } else {
            Drag::Pan {
                offset: (end - start) / 2.,
            }
        });
        self.follow = false;
        self.anchor = None;
        self.minimap_move(at);
    }

    fn minimap_move(&mut self, at: Point<Pixels>) {
        let Some(drag) = self.minimap_drag else {
            return;
        };
        let duration = f64::from(self.transport.duration);
        let time = self.minimap_time(at).clamp(0., duration);
        let width = f32::from(self.canvas.get().size.width);
        let minimum = f64::from(width / View::MAX_ZOOM);
        let maximum = f64::from(width / View::MIN_ZOOM).min(duration);
        let start = match drag {
            Drag::Pan { offset } => time - offset,
            Drag::Start { end, offset } => {
                let span = (end - (time - offset)).clamp(minimum.min(end), maximum.min(end));
                self.view.zoom =
                    (width / span.max(f64::EPSILON) as f32).clamp(View::MIN_ZOOM, View::MAX_ZOOM);
                end - f64::from(width / self.view.zoom)
            }
            Drag::End { start, offset } => {
                let available = (duration - start).max(f64::EPSILON);
                let span =
                    (time - offset - start).clamp(minimum.min(available), maximum.min(available));
                self.view.zoom =
                    (width / span.max(f64::EPSILON) as f32).clamp(View::MIN_ZOOM, View::MAX_ZOOM);
                start
            }
        };
        self.set_scroll(start as f32 * self.view.zoom);
    }
}

pub(super) fn element(state: &Editor, app: &Entity<Luma>) -> impl IntoElement {
    let envelope = state.overview_waveform.frame.clone();
    let visible = state.visible();
    let duration = f64::from(state.transport.duration);
    let playhead = state.transport.position;
    let loop_region = state.loop_region;
    let dragging = state.minimap_drag.is_some();
    let prepare = app.clone();
    let app = app.clone();
    div().h(px(HEIGHT)).flex_shrink_0().overflow_hidden().child(
        canvas(
            move |bounds, window, cx| {
                waveform::prepaint(&prepare, true, bounds, window, cx);
                agent_paint_node(Role::Slider, "Timeline minimap", bounds, window, cx);
                let (left, right) = lens(visible, duration, f32::from(bounds.size.width));
                for (label, x, width) in [
                    ("Minimap viewport", left, right - left),
                    ("Minimap start", left, HANDLE.min((right - left) / 2.)),
                    (
                        "Minimap end",
                        right - HANDLE.min((right - left) / 2.),
                        HANDLE.min((right - left) / 2.),
                    ),
                ] {
                    agent_paint_node(
                        Role::Slider,
                        label,
                        Bounds {
                            origin: point(bounds.origin.x + px(x), bounds.origin.y),
                            size: size(px(width), bounds.size.height),
                        },
                        window,
                        cx,
                    );
                }
                window.insert_hitbox(bounds, HitboxBehavior::Normal)
            },
            move |bounds, hitbox, window, _cx| {
                let width = f32::from(bounds.size.width);
                waveform::paint(bounds, envelope.as_deref(), None, window);
                let (left, right) = lens(visible, duration, width);
                let x = f32::from(window.mouse_position().x - bounds.origin.x);
                let handle = HANDLE.min((right - left) / 3.);
                let cursor = if dragging {
                    CursorStyle::ClosedHand
                } else if (x - left).abs().min((x - right).abs()) <= handle {
                    CursorStyle::ResizeLeftRight
                } else if x >= left && x <= right {
                    CursorStyle::OpenHand
                } else {
                    CursorStyle::PointingHand
                };
                window.set_cursor_style(cursor, &hitbox);

                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    let mut rect = |x: f32, y: f32, w: f32, h: f32, color: Hsla| {
                        if w > 0. && h > 0. {
                            window.paint_quad(fill(
                                Bounds {
                                    origin: point(bounds.origin.x + px(x), bounds.origin.y + px(y)),
                                    size: size(px(w), px(h)),
                                },
                                color,
                            ));
                        }
                    };
                    rect(0., 0., left, HEIGHT, fade(ladder::background(), 0.55));
                    rect(
                        right,
                        0.,
                        width - right,
                        HEIGHT,
                        fade(ladder::background(), 0.55),
                    );
                    rect(
                        left,
                        0.,
                        right - left,
                        HEIGHT,
                        fade(ladder::foreground(), 0.08),
                    );
                    if duration > 0. {
                        if let Some((start, end)) = loop_region {
                            let x = (start / duration) as f32 * width;
                            rect(
                                x,
                                0.,
                                ((end - start) / duration) as f32 * width,
                                HEIGHT,
                                fade(LOOP_BAND, 0.25),
                            );
                        }
                        rect(
                            playhead / duration as f32 * width,
                            0.,
                            1.,
                            HEIGHT,
                            ladder::playhead().into(),
                        );
                    }
                    let color: Hsla = ladder::primary().into();
                    rect(left, 0., right - left, 1., color);
                    rect(left, HEIGHT - 1., right - left, 1., color);
                    rect(left, 0., 2., HEIGHT, color);
                    rect((right - 2.).max(left), 0., 2., HEIGHT, color);
                });
                listen(&app, &hitbox, window);
            },
        )
        .size_full(),
    )
}

fn lens((start, end): (f64, f64), duration: f64, width: f32) -> (f32, f32) {
    if duration <= 0. {
        return (0., width);
    }
    (
        (start / duration).clamp(0., 1.) as f32 * width,
        (end / duration).clamp(0., 1.) as f32 * width,
    )
}

fn listen(app: &Entity<Luma>, hitbox: &Hitbox, window: &mut Window) {
    let pressed = app.clone();
    let hitbox = hitbox.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble
            && event.button == MouseButton::Left
            && hitbox.is_hovered(window)
        {
            pressed.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| editor.minimap_press(event.position))
            });
        }
    });
    let moved = app.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble {
            moved.update(cx, |this, cx| {
                if matches!(this.workspace.active_body(), Some(Body::TrackEditor(editor)) if editor.minimap_drag.is_some()) {
                    this.with_track_editor(cx, |editor| editor.minimap_move(event.position));
                }
            });
        }
    });
    let released = app.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
            released.update(cx, |this, cx| {
                if matches!(this.workspace.active_body(), Some(Body::TrackEditor(editor)) if editor.minimap_drag.is_some()) {
                    this.with_track_editor(cx, |editor| editor.minimap_drag = None);
                }
            });
        }
    });
}
