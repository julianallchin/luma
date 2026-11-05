//! Timeline geometry: the fixed ruler and waveform bands, the bottom-anchored
//! lane [`Layout`], and the [`View`] that maps time to pixels.

use super::*;

// -- geometry -----------------------------------------------------------------
//
// The layout at `zoomY == 1`, which is the only zoom the
// ruler and the waveform have. Vertical zoom scales the lanes and nothing
// else, so these are constants and [`Layout`] is what varies.

/// The ruler strip: `HEADER_HEIGHT`.
pub(super) const HEADER_HEIGHT: f32 = 32.;
/// `WAVEFORM_HEIGHT`. Fixed even under vertical zoom — it is a
/// navigation surface, not part of the annotation workspace.
pub(super) const WAVEFORM_HEIGHT: f32 = 80.;
/// `TRACK_HEIGHT`: one lane.
pub(super) const LANE_HEIGHT: f32 = 80.;
/// `layout.trackAreaY`: where the scrubbing surface ends and the lanes begin.
pub(super) const TRACK_AREA_Y: f32 = HEADER_HEIGHT + WAVEFORM_HEIGHT;
/// `ANNOTATION_HEADER_H`: the opaque strip at the top of a clip.
pub(super) const CLIP_HEADER: f32 = 18.;
/// The grab width of a clip's edge, in screen pixels.
pub(super) const HANDLE: f32 = 8.;
/// The ruler's and the clip labels' type size.
pub(super) const LABEL_SIZE: f32 = 10.;

/// Where the lane block sits on a canvas of a given height.
///
/// `computeBottomAnchoredLayout`: the lanes are pinned to the **floor** of the
/// viewport and grow upward, so z = 0 — the layer everything else is stacked
/// over — is always at the bottom edge and a new layer appears above what is
/// already there rather than pushing it down. Once there are more lanes than
/// the canvas is tall the ones that do not fit run off the *top*, under the
/// waveform, and [`View::lift`] is what reaches them: still bottom-anchored,
/// which is the whole point of anchoring it there.
///
/// Lanes expand to fill spare canvas height, so all visible editing space
/// responds to selection and insertion, including scores with only one layer.
#[derive(Clone, Copy)]
pub(super) struct Layout {
    /// The top of lane 0, the empty insertion lane. Negative once the lanes
    /// overflow the canvas and the block is sitting on the floor.
    pub(super) start: f32,
    /// One lane's height: `round(TRACK_HEIGHT * zoomY)`.
    pub(super) lane: f32,
    pub(super) rows: usize,
    /// The furthest the lanes may be lifted off the floor before the topmost
    /// one is fully on screen. Zero whenever they all fit.
    pub(super) max_lift: f32,
}

impl Layout {
    pub(super) fn new(rows: usize, height: f32, view: View) -> Self {
        let fitted = (height - TRACK_AREA_Y).max(0.) / rows.max(1) as f32;
        // Sparse scores use the whole editing area at the default zoom.
        // Explicit vertical zoom remains meaningful for larger arrangements.
        let lane = if rows <= 2 && view.zoom_y == 1. {
            fitted.max(1.)
        } else {
            (LANE_HEIGHT * view.zoom_y).round()
        };
        let natural = TRACK_AREA_Y + rows as f32 * lane;
        let max_lift = (natural - height).max(0.);
        Self {
            start: height - rows as f32 * lane + view.lift.clamp(0., max_lift),
            lane,
            rows,
            max_lift,
        }
    }

    /// The top of one lane.
    pub(super) fn top(self, row: usize) -> f32 {
        self.start + row as f32 * self.lane
    }

    /// The bottom edge of the lowest lane — the floor z = 0 sits on.
    fn floor(self) -> f32 {
        self.top(self.rows)
    }

    /// How many lanes there are between the floor and a point on the canvas.
    ///
    /// The anchor a vertical zoom holds: the lanes grow from the floor, so the
    /// fraction of a lane under the pointer is what has to survive a change of
    /// lane height.
    pub(super) fn rows_from_floor(self, y: f32) -> f32 {
        (self.floor() - y) / self.lane
    }

    /// Which lane a point `y` pixels down the canvas falls in, or `None` above
    /// the lanes and below the last of them.
    ///
    /// The waveform is the ceiling as well as [`Self::start`]: a lifted block
    /// runs *under* it, and a lane whose arithmetic reaches up there is one
    /// the pointer cannot see and must not answer for.
    pub(super) fn row_at(self, y: f32) -> Option<usize> {
        if y < TRACK_AREA_Y {
            return None;
        }
        let row = ((y - self.start).max(0.) / self.lane) as usize;
        (row < self.rows).then_some(row)
    }

    /// The nearest lane to a point, for a gesture that may not have started
    /// on one — a marquee dragged off the top or the bottom still has a row.
    pub(super) fn nearest_row(self, y: f32) -> usize {
        if y < self.start {
            return 0;
        }
        (((y - self.start) / self.lane) as usize).min(self.rows.saturating_sub(1))
    }

    /// The band of the canvas the lanes are allowed to paint in and answer the
    /// pointer from: everything below the fixed navigation surface.
    pub(super) fn band(self, canvas: Bounds<Pixels>) -> Bounds<Pixels> {
        Bounds {
            origin: point(canvas.origin.x, canvas.origin.y + px(TRACK_AREA_Y)),
            size: size(
                canvas.size.width,
                (canvas.size.height - px(TRACK_AREA_Y)).max(px(0.)),
            ),
        }
    }
}

/// Where the eye is: a horizontal zoom in pixels per second, and a scroll in
/// pixels.
#[derive(Clone, Copy)]
pub(super) struct View {
    pub(super) zoom: f32,
    pub(super) scroll: f32,
    /// `zoomY`: how tall a lane is, as a multiple of [`LANE_HEIGHT`]. The
    /// waveform and the ruler above it never scale with it — they are a
    /// navigation surface, not part of the annotation workspace.
    pub(super) zoom_y: f32,
    /// How far the lane block has been lifted off the canvas floor, in pixels.
    ///
    /// The vertical scroll, measured from the **bottom** rather than from the
    /// top, because the lanes are bottom-anchored: at zero, z = 0 sits on the
    /// floor. Stated this way a new layer, a vertical zoom or a resize keeps
    /// the floor where it is without anybody recomputing a scroll.
    pub(super) lift: f32,
}

impl View {
    /// Horizontal zoom limits in logical pixels per second. A long song can
    /// go below `MIN_ZOOM`, to fit it whole: see [`Editor::min_zoom`].
    pub(super) const MIN_ZOOM: f32 = 25.;
    pub(super) const MAX_ZOOM: f32 = 5_000.;
    /// The opening zoom.
    pub(super) const DEFAULT_ZOOM: f32 = 50.;
    /// `ZOOM_SENSITIVITY`: the exponential rate a modified wheel notch scales
    /// by.
    pub(super) const ZOOM_PER_PIXEL: f32 = 0.002;
    /// The rate a trackpad pinch scales by where it arrives as a ctrl-wheel
    /// (macOS sends a [`PinchEvent`] instead). Five times
    /// [`Self::ZOOM_PER_PIXEL`], because a pinch's deltas are a fifth the size
    /// of a wheel's.
    pub(super) const ZOOM_PER_PIXEL_PINCH: f32 = 0.01;
    /// `MIN_ZOOM_Y` / `MAX_ZOOM_Y`, and `ZOOM_Y_SENSITIVITY` for the alt-wheel
    /// that walks between them.
    pub(super) const MIN_ZOOM_Y: f32 = 0.5;
    pub(super) const MAX_ZOOM_Y: f32 = 1.5;
    pub(super) const ZOOM_Y_PER_PIXEL: f32 = 0.003;

    /// The time under a point `offset` pixels from the canvas's left edge.
    pub(super) fn time_at(self, offset: f32) -> f64 {
        (f64::from(offset) + f64::from(self.scroll)) / f64::from(self.zoom)
    }

    /// The time range a canvas `width` pixels wide shows.
    fn visible(&self, width: f32) -> (f64, f64) {
        (self.time_at(0.), self.time_at(width))
    }

    /// Where a time lands in logical pixels. Preserve subpixel motion until paint.
    pub(super) fn x_of(self, time: f64) -> f32 {
        (time * f64::from(self.zoom) - f64::from(self.scroll)) as f32
    }
}

/// How many lanes the canvas draws: the occupied ones plus the empty insertion
/// lane above them. A press below the last of them is a press on nothing, and
/// the lane stripes stop there too — one rule, so the paint and the hit test
/// cannot come to disagree about where the floor is.
pub(super) fn lane_count(clips: &[Clip]) -> usize {
    clips
        .iter()
        .map(|clip| clip.row + 1)
        .max()
        .unwrap_or(1)
        .max(1)
}

impl Editor {
    /// Where the lanes are, on the canvas the last frame painted.
    pub(super) fn layout(&self) -> Layout {
        Layout::new(
            lane_count(&self.clips),
            f32::from(self.canvas.get().size.height),
            self.view,
        )
    }

    /// The range on screen, from the canvas the last frame painted.
    pub(super) fn visible(&self) -> (f64, f64) {
        self.view.visible(f32::from(self.canvas.get().size.width))
    }
}
