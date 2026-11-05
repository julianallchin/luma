//! Where the eye goes: horizontal scroll and zoom, the lanes' lift and height,
//! and following the playhead.

use super::*;

/// A latched zoom anchor: the time held under a point on the canvas, and when
/// the gesture last fed it.
#[derive(Clone, Copy)]
pub(super) struct Anchor {
    offset: f32,
    time: f64,
    at: std::time::Instant,
}

/// How long a zoom gesture's anchor survives without another notch.
///
/// Long enough to cover a momentum flick, which keeps delivering after the
/// fingers lift. One number serves the modified wheel and the trackpad pinch,
/// whether it arrives as ctrl-wheel or as a [`PinchEvent`].
const ANCHOR_IDLE: Duration = Duration::from_millis(120);

/// What a wheel notch means, which is entirely a question of the modifier
/// held with it.
///
/// Named here rather than passed as a rate and a flag because the three are
/// exclusive and each takes a different axis: a call site that had to say
/// "this rate, but vertically" would be a call site that could say something
/// the canvas has no answer for.
#[derive(Clone, Copy)]
pub(super) enum Wheel {
    /// Bare: the scroll container's own gesture, both axes.
    Scroll,
    /// Horizontal zoom at an exponential rate — the platform key's, or the
    /// trackpad pinch's.
    Zoom(f32),
    /// `altKey`: vertical zoom, which is the lane height.
    Lanes,
}

impl Editor {
    /// Scroll the lanes, in pixels off the floor.
    ///
    /// The one place [`View::lift`] is written, so the bound a browser scroll
    /// container would apply for free is applied once here instead.
    pub(super) fn set_lift(&mut self, lift: f32) {
        self.view.lift = lift.clamp(0., self.layout().max_lift);
    }

    /// `altKey` wheel: taller or shorter lanes, holding whatever the pointer
    /// was over.
    ///
    /// The anchor is rows-from-the-floor rather than a pixel, because that is
    /// the quantity the bottom-anchored block preserves: zoom about a pixel and
    /// the floor would drift out from under z = 0.
    fn zoom_lanes(&mut self, delta: f32, at: f32) {
        // Above the lanes the gesture means nothing.
        if at < TRACK_AREA_Y {
            return;
        }
        let height = f32::from(self.canvas.get().size.height);
        let rows = self.layout().rows_from_floor(at);
        self.view.zoom_y =
            (self.view.zoom_y * delta.exp()).clamp(View::MIN_ZOOM_Y, View::MAX_ZOOM_Y);
        // The floor is `height + lift` by construction, so holding `rows`
        // lanes between it and the pointer is one line rather than a delta.
        self.set_lift(rows * self.layout().lane + at - height);
    }

    /// `H`: the lane height that fits every layer on the canvas, clamped to
    /// what the vertical zoom allows.
    ///
    /// No anchor, so the block goes back to sitting on the floor — which on a
    /// canvas too short even at the minimum is where the layer everything is
    /// stacked over belongs.
    fn fit_lanes(&mut self) {
        let height = f32::from(self.canvas.get().size.height);
        let rows = lane_count(&self.clips) as f32;
        self.view.zoom_y = ((height - TRACK_AREA_Y) / (rows * LANE_HEIGHT))
            .clamp(View::MIN_ZOOM_Y, View::MAX_ZOOM_Y);
        self.view.lift = 0.;
    }

    /// The furthest the view may scroll: the content's width less the
    /// canvas's. A browser scroll container applies this bound for free; here
    /// it is the one place scroll is written, so it applies it once.
    /// The lowest horizontal zoom: the whole song across the canvas, or
    /// [`View::MIN_ZOOM`] when that is further in.
    pub(super) fn min_zoom(&self) -> f32 {
        let width = f32::from(self.canvas.get().size.width);
        let duration = self.transport.duration;
        if duration > 0. && width > 0. {
            View::MIN_ZOOM.min(width / duration)
        } else {
            View::MIN_ZOOM
        }
    }

    pub(super) fn set_scroll(&mut self, scroll: f32) {
        let content = f64::from(self.transport.duration).max(0.) as f32 * self.view.zoom;
        let width = f32::from(self.canvas.get().size.width);
        self.view.scroll = scroll.clamp(0., (content - width).max(0.));
    }

    /// Advance wheel zoom while keeping its latched time under the pointer.
    pub(super) fn tick_zoom(&mut self, now: std::time::Instant) -> bool {
        let Some(motion) = &mut self.zoom_motion else {
            return false;
        };
        let (zoom, settled) = motion.advance(now);
        self.view.zoom = zoom;
        if let Some(anchor) = self.anchor {
            self.set_scroll(anchor.time as f32 * zoom - anchor.offset);
        }
        if settled {
            self.zoom_motion = None;
        }
        true
    }

    /// Centre the view on the playhead, if the eye is following it.
    pub(super) fn follow_playhead(&mut self) {
        if !self.follow {
            return;
        }
        let width = f32::from(self.canvas.get().size.width);
        self.set_scroll(self.transport.position * self.view.zoom - width / 2.);
    }
}

impl Luma {
    /// Toggle following the playhead, and take up the new setting at once —
    /// turning it on with the transport stopped should still centre what is
    /// already on screen.
    pub(crate) fn toggle_follow(&mut self, cx: &mut Context<Self>) {
        self.with_track_editor(cx, |editor| {
            editor.follow = !editor.follow;
            editor.follow_playhead();
        });
    }

    /// `H`: fit every lane on the canvas.
    pub(crate) fn fit_lanes(&mut self, cx: &mut Context<Self>) {
        self.with_track_editor(cx, Editor::fit_lanes);
    }

    /// A wheel notch over the canvas.
    ///
    /// A bare wheel scrolls both axes, because this canvas is its own scroll
    /// container. A modified wheel zooms, and
    /// the horizontal zoom is anchored on a *latched* point: whatever was
    /// under the pointer when the gesture started stays under it until the
    /// wheel goes quiet for [`ANCHOR_IDLE`]. Recomputing the anchor per event
    /// is what lets a momentum flick walk it across the track.
    pub(super) fn timeline_wheel(
        &mut self,
        at: Point<Pixels>,
        delta: Point<f32>,
        wheel: Wheel,
        cx: &mut Context<Self>,
    ) {
        self.with_timeline_view(cx, |editor| {
            let canvas = editor.canvas.get();
            match wheel {
                Wheel::Scroll => {
                    editor.anchor = None;
                    editor.zoom_motion = None;
                    let scroll = editor.view.scroll - delta.x;
                    editor.set_scroll(scroll);
                    editor.set_lift(editor.view.lift + delta.y);
                    return;
                }
                Wheel::Lanes => {
                    editor.anchor = None;
                    editor.zoom_motion = None;
                    editor.zoom_lanes(
                        delta.y * View::ZOOM_Y_PER_PIXEL,
                        f32::from(at.y - canvas.origin.y),
                    );
                    return;
                }
                Wheel::Zoom(_) => {}
            }
        });
        if let Wheel::Zoom(rate) = wheel {
            // Exponential in the scroll distance, so a fast flick and a slow
            // one over the same distance land in the same place.
            self.timeline_zoom(at, delta.y * rate, cx);
        }
    }

    /// Scale the horizontal zoom by `e^amount` about the pointer: positive
    /// zooms in. The wheel and the pinch both end here, so they share the
    /// latched anchor, the clamp and the eased motion.
    ///
    /// In log space because that is what the eased motion integrates, and
    /// because it makes successive steps add: a pinch's `1 + delta` factors
    /// and a wheel's exponential notches compose the same way.
    pub(super) fn timeline_zoom(&mut self, at: Point<Pixels>, amount: f32, cx: &mut Context<Self>) {
        if amount == 0. || !amount.is_finite() {
            return;
        }
        let reduced_motion = cx.reduce_motion();
        self.with_timeline_view(cx, |editor| {
            let offset = f32::from(at.x - editor.canvas.get().origin.x);
            let now = std::time::Instant::now();
            editor.tick_zoom(now);
            let anchor = match editor.anchor {
                Some(anchor) if now.duration_since(anchor.at) < ANCHOR_IDLE => anchor,
                _ => Anchor {
                    offset,
                    time: editor.view.time_at(offset),
                    at: now,
                },
            };
            if reduced_motion {
                editor.zoom_motion = None;
                editor.view.zoom =
                    (editor.view.zoom * amount.exp()).clamp(editor.min_zoom(), View::MAX_ZOOM);
                editor.set_scroll(anchor.time as f32 * editor.view.zoom - anchor.offset);
            } else {
                let floor = editor.min_zoom();
                editor
                    .zoom_motion
                    .get_or_insert_with(|| zoom_motion::Zoom::new(editor.view.zoom, floor, now))
                    .push(amount);
            }
            editor.anchor = Some(Anchor { at: now, ..anchor });
        });
    }

    /// Edit what only the timeline surface draws — where it looks, how far
    /// in — and redraw that surface alone. A notify of the app rebuilds every
    /// cached region in the window, once per wheel event.
    fn with_timeline_view(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Editor)) {
        let Some(Body::TrackEditor(state)) = self.workspace.active_body_mut() else {
            return;
        };
        edit(state);
        match state.playback_surface.clone() {
            Some(surface) => surface.update(cx, |_, cx| cx.notify()),
            None => cx.notify(),
        }
    }
}
