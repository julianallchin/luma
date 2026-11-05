//! The timeline canvas element: the frame's [`Scene`], clip boxes, and the
//! automation nodes a script acts on.

use super::*;

/// One element for the whole timeline.
///
/// Everything the paint needs is captured by value — one refcounted clip list,
/// one refcounted waveform, one `Copy` view — so a frame draws a consistent
/// picture without reaching back into the app to ask what it looks like. The
/// pointer handlers do the reverse: they carry no picture at all, only the
/// entity to send the gesture to, because by the time one runs the frame it
/// was registered in is already gone.
pub(super) fn canvas_element(state: &Editor, app: &Entity<Luma>) -> impl IntoElement {
    // Present a completed physical-pixel waveform and its camera together.
    // This adds one pipeline frame of latency, without resampling the texture
    // or letting the grid run ahead of the waveform during focus playback.
    let (view, playhead) = state
        .timeline_waveform
        .frame
        .as_ref()
        .and_then(|frame| frame.camera)
        .filter(|(view, _)| state.follow && state.transport.playing && view.zoom == state.view.zoom)
        .map(|(view, position)| {
            // At either scroll limit the waveform is stationary and reusable;
            // its original capture time must not freeze the moving playhead.
            let position = if view.scroll == state.view.scroll {
                state.transport.position
            } else {
                position
            };
            (view, position)
        })
        .unwrap_or((state.view, state.transport.position));
    let scene = Scene {
        clips: Rc::clone(&state.clips),
        previews: Rc::clone(&state.previews),
        waveform: state.timeline_waveform.frame.clone(),
        beats: state.beats.clone(),
        view,
        playhead,
        selected: state.selected.clone(),
        cursor: state.cursor,
        loop_region: state.loop_region,
        menu: state.menu,
        ghost: state.drop_ghost().map(Rc::new),
    };
    let registered = scene.clone();
    // Over an alpha line, the cursor says what a drag does. A drag keeps it
    // wherever the pointer goes.
    let cursor = match &state.gesture {
        Some(Gesture::Alpha { grab, .. }) => Some((grab.part.cursor(true), true)),
        Some(Gesture::Pan { .. }) => Some((CursorStyle::ClosedHand, true)),
        Some(_) => None,
        None => state.alpha_hover.map(|part| (part.cursor(false), false)),
    };
    let canvas_bounds = Rc::clone(&state.canvas);
    let app = app.clone();
    let resized = app.clone();
    let dropped = app.clone();
    let carried = app.clone();

    div()
        .flex_1()
        .overflow_hidden()
        // A preset carried from the browser shows where it would land while
        // it is over the timeline, and lands there when it is let go.
        .on_drag_move(move |event: &DragMoveEvent<sheet::PresetDrag>, _, cx| {
            let at = event.event.position;
            let over = event.bounds.contains(&at).then_some(at);
            let drag = *event.drag(cx);
            carried.update(cx, |this, cx| this.carry_preset(&drag, over, cx));
        })
        .on_drop(move |drag: &sheet::PresetDrag, window, cx| {
            let at = window.mouse_position();
            dropped.update(cx, |this, cx| this.drop_preset(drag, at, cx));
        })
        .child(
            canvas(
                move |bounds, window, cx| {
                    // Where the canvas ended up is what turns a window-space mouse
                    // position back into a time, and only prepaint knows it. A
                    // press can arrive before the next paint but never before the
                    // next prepaint, so this is also the only place it is safe to
                    // write.
                    //
                    canvas_bounds.set(bounds);
                    waveform::prepaint(&resized, false, bounds, window, cx);
                    register(&registered, bounds, window, cx);
                    window.insert_hitbox(bounds, HitboxBehavior::Normal)
                },
                move |bounds, hitbox, window, cx| {
                    draw::paint(bounds, &scene, window, cx);
                    match cursor {
                        Some((style, true)) => window.set_window_cursor_style(style),
                        Some((style, false)) => window.set_cursor_style(style, &hitbox),
                        None => {}
                    }
                    listen(&app, &hitbox, window);
                },
            )
            .size_full(),
        )
}

/// Everything one frame draws, resolved and refcounted.
#[derive(Clone)]
pub(super) struct Scene {
    pub(super) clips: Rc<[Clip]>,
    pub(super) previews: Rc<RefCell<HashMap<SharedString, Installed>>>,
    pub(super) waveform: Option<Rc<waveform::Painted>>,
    pub(super) beats: Option<Rc<BeatGrid>>,
    pub(super) view: View,
    pub(super) playhead: f32,
    pub(super) selected: Vec<SharedString>,
    pub(super) cursor: Option<Cursor>,
    pub(super) loop_region: Option<(f64, f64)>,
    pub(super) menu: Option<InsertMenu>,
    /// A preset carried over the timeline from the browser.
    pub(super) ghost: Option<Rc<sheet::DropGhost>>,
}

impl Scene {
    /// Where the lanes sit on this canvas. Derived rather than carried: it is
    /// a function of the clip list and the height, and a frame that stored it
    /// could disagree with the frame that drew it.
    pub(super) fn layout(&self, canvas: Bounds<Pixels>) -> Layout {
        Layout::new(
            lane_count(&self.clips),
            f32::from(canvas.size.height),
            self.view,
        )
    }

    fn playhead_box(&self, canvas: Bounds<Pixels>) -> Bounds<Pixels> {
        Bounds {
            origin: point(
                canvas.origin.x + px(self.view.x_of(f64::from(self.playhead))),
                canvas.origin.y,
            ),
            size: size(px(1.), canvas.size.height),
        }
    }
}

/// One clip's box in window space, for a canvas at `canvas`.
pub(super) fn clip_bounds(
    view: View,
    layout: Layout,
    canvas: Bounds<Pixels>,
    clip: &Clip,
) -> Bounds<Pixels> {
    let x = view.x_of(clip.start);
    let width = ((clip.end - clip.start) as f32 * view.zoom).floor().max(4.);
    Bounds {
        origin: point(
            canvas.origin.x + px(x),
            canvas.origin.y + px(layout.top(clip.row) + 1.),
        ),
        size: size(px(width), px(layout.lane - 2.)),
    }
}

/// Name what a script can act on: the scrubbing surface, every clip, both of
/// every clip's edge handles, and the playhead.
///
/// The handles are their own nodes because a clip's *centre* — which is where
/// the harness clicks and where a drag starts from — is nowhere near either
/// edge, so a script could not otherwise reach the one control this screen
/// exists to offer.
fn register(scene: &Scene, canvas: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    // Two surfaces, not one: the ruler scrubs and the waveform clears the
    // selection, so a script that means to move the playhead has somewhere to
    // press that is not the waveform.
    agent_paint_node(
        Role::Card,
        "Ruler",
        Bounds {
            origin: canvas.origin,
            size: size(canvas.size.width, px(HEADER_HEIGHT)),
        },
        window,
        cx,
    );
    agent_paint_node(
        Role::Card,
        "Waveform",
        Bounds {
            origin: point(canvas.origin.x, canvas.origin.y + px(HEADER_HEIGHT)),
            size: size(canvas.size.width, px(WAVEFORM_HEIGHT)),
        },
        window,
        cx,
    );
    // Everything below is on the lane surface, which the fixed navigation
    // surface above it covers once the lanes are taller than the canvas. The
    // mask is what tells a script so: a node the waveform hides collapses to
    // no height, which is already the harness's word for "there is no point on
    // screen that would hit this".
    let layout = scene.layout(canvas);
    window.with_content_mask(
        Some(ContentMask {
            bounds: layout.band(canvas),
        }),
        |window| {
            // Each lane as a row, so the empty space between clips is
            // addressable. A press there is a real gesture — it sweeps a range
            // — and a surface a script cannot name is a gesture it cannot make.
            for lane in 0..layout.rows {
                agent_paint_node(
                    Role::Row,
                    format!("Lane {lane}"),
                    Bounds {
                        origin: point(canvas.origin.x, canvas.origin.y + px(layout.top(lane))),
                        size: size(canvas.size.width, px(layout.lane)),
                    },
                    window,
                    cx,
                );
            }
            for clip in scene.clips.iter() {
                let box_ = clip_bounds(scene.view, layout, canvas, clip);
                // The *grabbable* extent, not the drawn one: only a clip's
                // header bar answers the pointer, so a node covering its whole
                // lane would send every scripted click into the inert body.
                let header = Bounds {
                    origin: box_.origin,
                    size: size(box_.size.width, px(CLIP_HEADER)),
                };
                agent_paint_node(Role::Card, clip.label.clone(), header, window, cx);
                fades::register(box_, clip, window, cx);
                // The body stays inert to the pointer; this node is evidence,
                // not a control. It exists exactly when the clip has a decoded
                // heatmap, which is the only way a headless probe can tell a
                // preview surface from the flat fallback fill.
                if scene.previews.borrow().contains_key(&clip.id) {
                    agent_paint_node(
                        Role::Card,
                        format!("{} preview", clip.label),
                        clip_body(box_),
                        window,
                        cx,
                    );
                }
                for edge in [Edge::Start, Edge::End] {
                    let x = match edge {
                        Edge::Start => box_.origin.x,
                        Edge::End => box_.origin.x + box_.size.width - px(HANDLE),
                    };
                    agent_paint_node(
                        Role::Slider,
                        format!("{} {}", clip.label, edge.suffix()),
                        Bounds {
                            origin: point(x, header.origin.y),
                            size: size(px(HANDLE), header.size.height),
                        },
                        window,
                        cx,
                    );
                }
            }
            // A carried preset is evidence of where a drop would land.
            if let Some(ghost) = &scene.ghost {
                agent_paint_node(
                    Role::Card,
                    format!("{} drop preview", ghost.clip.label),
                    ghost_box(canvas, layout, scene, ghost),
                    window,
                    cx,
                );
            }
            // The cursor is a control in the sense that matters here: it is
            // where the next edit lands, and nothing else on the canvas
            // reports it.
            if let Some(cursor) = scene.cursor {
                let (min_row, max_row) = cursor.rows();
                let (from, to) = cursor.span().unwrap_or((cursor.start, cursor.start));
                let left = scene.view.x_of(from);
                agent_paint_node(
                    Role::Slider,
                    "Cursor",
                    Bounds {
                        origin: point(
                            canvas.origin.x + px(left),
                            canvas.origin.y + px(layout.top(min_row)),
                        ),
                        size: size(
                            px((scene.view.x_of(to) - left).max(2.)),
                            px((max_row - min_row + 1) as f32 * layout.lane),
                        ),
                    },
                    window,
                    cx,
                );
            }
        },
    );
    // A slider is the closest thing in the closed role vocabulary to a mark
    // whose position along an axis *is* its value, which is what a script
    // watches to know where the transport got to.
    agent_paint_node(
        Role::Slider,
        "Playhead",
        scene.playhead_box(canvas),
        window,
        cx,
    );
}
