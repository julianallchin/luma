//! Where `count` fixtures sit along a host feature, and what to do when they
//! do not fit.
//!
//! Pure arithmetic over one line segment: no catalog, no database, no venue.
//! The caller ([`crate::venue::place_on`]) turns an offset into a pose, because
//! an offset on a host face *is* `u`, in metres, from the face's middle — that
//! is what makes this module small enough to be total.
//!
//! # The rule, whole
//!
//! A distribution occupies a **band**: `count` bodies of `width` metres each,
//! laid out centre-to-centre inside a segment of the host feature. Every
//! [`Layout`] is the same band with a different thing held fixed —
//!
//! - [`Layout::Even`] fixes the *segment* (the whole face) and derives the
//!   spacing, with a half-body margin at each end so the outer two bodies sit
//!   inside the face rather than half off it;
//! - [`Layout::Spacing`] fixes the *pitch* and derives the band, centred on the
//!   segment;
//! - [`Layout::Span`] narrows the segment to a window of the feature, in metres
//!   off its middle, and is then [`Layout::Even`] within it;
//! - [`Layout::At`] fixes the band's *centre* and packs the bodies.
//!
//! — so there is one fit test and one centring rule, not three.
//!
//! # Fit is a refusal, never a squeeze
//!
//! A band longer than its segment is [`Fit::TooLong`]: nothing is placed, and
//! the report says how long the feature would have to be. Clipping, dropping
//! the overflow, or letting bodies overlap are all ways of answering a question
//! nobody asked — the human asked for eight movers on this truss, and if eight
//! do not fit the answer is the truss, not seven movers.
//!
//! [`Fit::TooLong::needed_m`] is quantized *up* to whatever length the host can
//! actually be built at ([`Feature::quantum_m`]), so the number in the refusal
//! is a number the caller can feed straight back into the host's `span` and
//! re-run. A need of 4.1 m on a truss that comes in half-metre panels is
//! reported as 4.5 m, because 4.1 m of truss does not exist.

/// The host feature a distribution runs along: how long it is, and how short a
/// change to that length the host admits.
///
/// Not a socket and not a node — the geometry is upstream's
/// (`luma_render::face`), and what the arithmetic needs from it is one length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Feature {
    /// Metres along the feature, or `None` where it is unbounded — the venue
    /// floor and grid are planes, not pieces, and nothing overruns them.
    pub length_m: Option<f64>,
    /// The shortest increment the host can be built in — 0.5 m for a truss,
    /// which comes in panels. `None` for a host whose length is not a
    /// parameter anybody can change (a measured GLB, the floor).
    pub quantum_m: Option<f64>,
}

impl Feature {
    /// A bounded feature of `length_m`, changeable only in whole `quantum_m`.
    #[must_use]
    pub fn bounded(length_m: f64, quantum_m: Option<f64>) -> Feature {
        Feature {
            length_m: Some(length_m),
            quantum_m,
        }
    }

    /// A feature nothing can overrun.
    #[must_use]
    pub fn unbounded() -> Feature {
        Feature {
            length_m: None,
            quantum_m: None,
        }
    }

    /// The shortest buildable length that is at least `metres`.
    ///
    /// The whole reason a refusal's `needed_m` is usable: a caller that sets
    /// the host's length to this number and asks again gets a yes.
    #[must_use]
    pub fn buildable(&self, metres: f64) -> f64 {
        match self.quantum_m {
            Some(quantum) if quantum > 0.0 => (metres / quantum).ceil() * quantum,
            _ => metres,
        }
    }
}

/// How the caller pinned the layout down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layout {
    /// Evenly across the whole feature, with a half-body margin at each end.
    Even,
    /// A fixed centre-to-centre pitch, the band centred on the feature.
    Spacing(f64),
    /// Evenly across the window `a..b`, in **metres off the feature's middle**
    /// — the same origin every other number in this module is measured from, so
    /// `Span(-4.0, 4.0)` is the middle eight metres whatever the host's length.
    ///
    /// Reversed bounds are ordered and out-of-range ones are clamped to the
    /// feature rather than refused: a dragged handle produces both, and neither
    /// is a mistake anybody can act on.
    Span(f64, f64),
    /// Packed body to body, the band centred `metres` from the feature's
    /// **middle** — the fourth thing a caller can hold fixed, and the one that
    /// makes "this light, here" a distribution rather than a second verb.
    ///
    /// Signed, because the middle is the origin the whole module measures from:
    /// `At(0.0)` is dead centre and `At(-2.0)` is two metres toward the
    /// feature's negative-tangent end.
    At(f64),
}

/// Why a distribution will not fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fit {
    /// The band is longer than the segment it had to sit in.
    TooLong {
        /// How long the **whole feature** would have to be, already rounded up
        /// to a length the host can be built at.
        needed_m: f64,
        /// How long the whole feature is now.
        available_m: f64,
    },
}

/// Where each body's centre sits, in metres along the feature's tangent from
/// its middle — ascending, which is also physical order along the host.
///
/// # Errors
/// [`Fit::TooLong`] if the band will not fit; nothing is returned, because a
/// partial distribution is not one.
///
/// # Panics
/// Never: `width_m` and a `Layout`'s numbers are sanitized here rather than
/// refused, so every input has an answer.
pub fn offsets(
    feature: Feature,
    layout: Layout,
    count: usize,
    width_m: f64,
) -> Result<Vec<f64>, Fit> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let width = if width_m.is_finite() && width_m > 0.0 {
        width_m
    } else {
        0.0
    };
    // The band is what has to fit; on an unbounded feature it is also the whole
    // segment, so nothing below needs a second branch for the floor and every
    // layout keeps meaning what it means there.
    let band = band_length(layout, count, width);
    let (segment_start, segment_length) = match (feature.length_m, layout) {
        (Some(length), _) => segment(length, layout),
        // Nothing overruns a plane, so a window on one is exactly the window.
        (None, Layout::Span(a, b)) => {
            let (lo, hi) = ordered(a, b);
            (lo, hi - lo)
        }
        (None, _) => (-band / 2.0, band),
    };
    if band > segment_length + FIT_EPSILON_M {
        return Err(too_long(feature, layout, band));
    }

    let pitch = match layout {
        Layout::Spacing(spacing) => spacing.abs(),
        // Packed: `At` fixes where the band sits, so the only thing left for it
        // to derive is nothing at all.
        Layout::At(_) => width,
        // The outer bodies keep a half-width margin, so what the interior
        // divides is the segment minus one whole body.
        Layout::Even | Layout::Span(_, _) if count > 1 => {
            (segment_length - width) / (count - 1) as f64
        }
        Layout::Even | Layout::Span(_, _) => 0.0,
    };

    // Centre the row on the segment by its own extent — which for `Even` is the
    // segment itself, and for `Spacing` is the short band a long truss carries
    // in its middle rather than against one end.
    let extent = (count - 1) as f64 * pitch + width;
    let centre = match layout {
        Layout::At(metres) if metres.is_finite() => metres,
        Layout::At(_) => 0.0,
        _ => segment_start + segment_length / 2.0,
    };
    let first = centre - extent / 2.0 + width / 2.0;
    // An `At` band can sit inside a feature it still overruns, because the
    // caller chose where rather than how much. That is the one fit question the
    // band-versus-segment test above cannot ask.
    if let (Layout::At(_), Some(length)) = (layout, feature.length_m) {
        let reach = (centre.abs() + extent / 2.0) * 2.0;
        if reach > length + FIT_EPSILON_M {
            return Err(Fit::TooLong {
                needed_m: feature.buildable(reach),
                available_m: length,
            });
        }
    }
    Ok((0..count).map(|i| first + pitch * i as f64).collect())
}

/// Slack, in metres, below which a band counts as fitting. A truss panel is
/// half a metre and a bolt hole is millimetres; a micron of float drift is
/// neither.
const FIT_EPSILON_M: f64 = 1e-9;

/// The segment a bounded feature offers: `(start from its middle, length)`.
fn segment(length: f64, layout: Layout) -> (f64, f64) {
    match layout {
        Layout::Even | Layout::Spacing(_) => (-length / 2.0, length),
        Layout::Span(a, b) => {
            let (lo, hi) = ordered(a, b);
            let half = length / 2.0;
            let (lo, hi) = (lo.clamp(-half, half), hi.clamp(-half, half));
            (lo, hi - lo)
        }
        // The band *is* the segment: `At` says where it goes, and the fit test
        // below is then "does that land on the host", which is the only way an
        // `At` can be too long.
        Layout::At(_) => (-length / 2.0, length),
    }
}

/// A window's two bounds, in order, with a non-finite one read as the middle.
fn ordered(a: f64, b: f64) -> (f64, f64) {
    let keep = |t: f64| if t.is_finite() { t } else { 0.0 };
    let (a, b) = (keep(a), keep(b));
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// How much of the feature the bodies claim end to end.
fn band_length(layout: Layout, count: usize, width: f64) -> f64 {
    match layout {
        // `Even` and `Span` derive their pitch from the segment, so their band
        // *is* the segment as long as the bodies themselves fit in it.
        Layout::Even | Layout::Span(_, _) | Layout::At(_) => count as f64 * width,
        Layout::Spacing(spacing) => (count - 1) as f64 * spacing.abs() + width,
    }
}

/// The refusal: how long the whole feature would have to be for this band.
///
/// A `Span` names a window in metres, so lengthening the host only helps while
/// the window is still hanging off the end of it — past that the fix is a wider
/// window, and the number reported is the length that at least stops clipping.
fn too_long(feature: Feature, layout: Layout, band: f64) -> Fit {
    let available = feature.length_m.unwrap_or(f64::INFINITY);
    let whole = match layout {
        Layout::Even | Layout::Spacing(_) | Layout::At(_) => band,
        Layout::Span(a, b) => {
            let (lo, hi) = ordered(a, b);
            (2.0 * lo.abs().max(hi.abs())).max(band)
        }
    };
    Fit::TooLong {
        needed_m: feature.buildable(whole),
        available_m: available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A metre-wide truss face in half-metre panels, and a 0.3 m body.
    const PANEL: Option<f64> = Some(0.5);
    const BODY: f64 = 0.3;

    fn spread(length: f64, layout: Layout, count: usize) -> Result<Vec<f64>, Fit> {
        offsets(Feature::bounded(length, PANEL), layout, count, BODY)
    }

    /// The load-bearing property: whatever the layout, the row is symmetric
    /// about the feature's middle and stays inside it. Asserting the midpoint
    /// rather than a literal is what keeps this a measurement.
    #[test]
    fn every_layout_centres_its_row_inside_the_feature() {
        for layout in [Layout::Even, Layout::Spacing(0.4), Layout::Span(-2.0, 2.0)] {
            for count in 1..=8 {
                let row = spread(4.0, layout, count).expect("four metres holds eight bodies");
                assert_eq!(row.len(), count);
                let midpoint = (row[0] + row[row.len() - 1]) / 2.0;
                assert!(
                    midpoint.abs() < 1e-9,
                    "{layout:?} x{count} is off centre by {midpoint}"
                );
                assert!(
                    row[0] - BODY / 2.0 >= -2.0 - 1e-9,
                    "{layout:?} x{count} hangs off the near end"
                );
                assert!(
                    row[row.len() - 1] + BODY / 2.0 <= 2.0 + 1e-9,
                    "{layout:?} x{count} hangs off the far end"
                );
            }
        }
    }

    #[test]
    fn one_body_sits_in_the_middle() {
        for layout in [Layout::Even, Layout::Spacing(0.6), Layout::Span(-2.0, 2.0)] {
            let row = spread(4.0, layout, 1).unwrap();
            assert!(
                row[0].abs() < 1e-9,
                "{layout:?} put the only body at {row:?}"
            );
        }
    }

    /// Nothing asked for, nothing placed — and no error either. A count of zero
    /// is a distribution of no fixtures, which is exactly what it does.
    #[test]
    fn no_bodies_is_an_empty_row_not_a_refusal() {
        assert_eq!(spread(4.0, Layout::Even, 0).unwrap(), Vec::<f64>::new());
    }

    #[test]
    fn spacing_is_the_pitch_it_says_it_is() {
        let row = spread(4.0, Layout::Spacing(0.75), 5).unwrap();
        for pair in row.windows(2) {
            assert!((pair[1] - pair[0] - 0.75).abs() < 1e-9, "{row:?}");
        }
    }

    /// `Even` spreads to the ends: outer centres exactly a half body inside.
    #[test]
    fn even_reaches_both_margins() {
        let row = spread(4.0, Layout::Even, 4).unwrap();
        assert!((row[0] - (-2.0 + BODY / 2.0)).abs() < 1e-9, "{row:?}");
        assert!((row[3] - (2.0 - BODY / 2.0)).abs() < 1e-9, "{row:?}");
    }

    /// A row on the floor has no ends to spread to, so `Even` there is the
    /// tightest honest answer: bodies touching, centred on the origin.
    #[test]
    fn even_on_an_unbounded_feature_packs_shoulder_to_shoulder() {
        let row = offsets(Feature::unbounded(), Layout::Even, 4, BODY).unwrap();
        for pair in row.windows(2) {
            assert!((pair[1] - pair[0] - BODY).abs() < 1e-9, "{row:?}");
        }
    }

    /// A span is the same rule over a sub-segment: the row's midpoint moves to
    /// the sub-segment's midpoint, not the feature's.
    #[test]
    fn a_span_lays_out_inside_its_window() {
        // The outer half of a four-metre face: metres off the middle, like
        // every other number here.
        let row = spread(4.0, Layout::Span(0.0, 2.0), 3).unwrap();
        let midpoint = (row[0] + row[2]) / 2.0;
        assert!((midpoint - 1.0).abs() < 1e-9, "{row:?}");
        assert!(row[0] >= 0.0 - 1e-9 && row[2] <= 2.0 + 1e-9, "{row:?}");
    }

    /// A window wider than the host is clipped to it rather than refused: the
    /// caller asked for "as much of the middle as there is".
    #[test]
    fn a_span_wider_than_the_feature_is_the_feature() {
        assert_eq!(
            spread(4.0, Layout::Span(-50.0, 50.0), 4).unwrap(),
            spread(4.0, Layout::Even, 4).unwrap()
        );
    }

    #[test]
    fn a_reversed_span_is_the_span_it_names() {
        assert_eq!(
            spread(4.0, Layout::Span(2.0, 0.0), 3).unwrap(),
            spread(4.0, Layout::Span(0.0, 2.0), 3).unwrap()
        );
    }

    /// Exact fit is a fit: eight 0.5 m bodies on 4 m place, nine do not.
    #[test]
    fn an_exact_fit_is_admitted_and_one_more_is_not() {
        let feature = Feature::bounded(4.0, PANEL);
        assert!(offsets(feature, Layout::Even, 8, 0.5).is_ok());
        assert!(offsets(feature, Layout::Even, 9, 0.5).is_err());
    }

    /// The acceptance property, at this layer: the length a refusal asks for is
    /// a length that makes the same call succeed. Swept, so it holds for every
    /// count rather than for the one somebody picked.
    #[test]
    fn the_stated_need_is_what_makes_the_call_succeed() {
        // `Span` is deliberately absent: its window is metres rather than a
        // fraction, so a host that grew would still offer the same window and
        // the property this sweeps for is not one it claims.
        for layout in [Layout::Even, Layout::Spacing(0.7)] {
            for count in 2..=12 {
                let short = Feature::bounded(1.0, PANEL);
                let Err(Fit::TooLong {
                    needed_m,
                    available_m,
                }) = offsets(short, layout, count, BODY)
                else {
                    continue;
                };
                assert_eq!(available_m, 1.0);
                assert!(
                    offsets(Feature::bounded(needed_m, PANEL), layout, count, BODY).is_ok(),
                    "{layout:?} x{count} asked for {needed_m} m and still would not fit"
                );
            }
        }
    }

    /// A truss comes in half-metre panels, so a need of 4.1 m is reported as
    /// 4.5 m — the shortest truss that exists and is long enough. Reporting the
    /// raw 4.1 would hand back a number that quantizes *down* to 4.0 and fails
    /// on the retry.
    #[test]
    fn the_stated_need_is_a_length_the_host_can_be_built_at() {
        let Err(Fit::TooLong { needed_m, .. }) =
            offsets(Feature::bounded(1.0, PANEL), Layout::Even, 12, 0.34)
        else {
            panic!("twelve 0.34 m bodies do not fit on a metre");
        };
        assert!((needed_m - 4.5).abs() < 1e-9, "{needed_m}");
    }

    /// Without a quantum the need is the raw need — a measured GLB is the
    /// length it is, and rounding it up would invent a piece.
    #[test]
    fn a_host_with_no_quantum_states_its_raw_need() {
        let Err(Fit::TooLong { needed_m, .. }) =
            offsets(Feature::bounded(1.0, None), Layout::Even, 12, 0.34)
        else {
            panic!("twelve 0.34 m bodies do not fit on a metre");
        };
        assert!((needed_m - 4.08).abs() < 1e-9, "{needed_m}");
    }

    /// A span asked for a quarter of the feature, so extending it to the band's
    /// own length would still leave the band overrunning that quarter.
    #[test]
    fn a_span_hanging_off_the_end_asks_for_the_length_that_holds_it() {
        // A window from 1 m to 3 m off the middle needs a six-metre face to be
        // fully on the host at all.
        let Err(Fit::TooLong { needed_m, .. }) =
            offsets(Feature::bounded(2.0, None), Layout::Span(1.0, 3.0), 4, 0.25)
        else {
            panic!("the window hangs off a two-metre face");
        };
        assert!((needed_m - 6.0).abs() < 1e-9, "{needed_m}");
    }

    /// The floor is a plane. Nothing overruns it, at any count or pitch.
    #[test]
    fn an_unbounded_feature_never_refuses() {
        let row = offsets(Feature::unbounded(), Layout::Spacing(2.0), 50, BODY).unwrap();
        assert_eq!(row.len(), 50);
        assert!(((row[0] + row[49]) / 2.0).abs() < 1e-9, "still centred");
    }

    /// Spacing tighter than the bodies is the caller's business — real rigs
    /// hang pars shoulder to shoulder — but the fit test still measures the
    /// band it produces, not the pitch alone.
    #[test]
    fn spacing_under_the_body_width_still_fits_by_its_band() {
        let row = offsets(Feature::bounded(1.0, PANEL), Layout::Spacing(0.1), 6, 0.3).unwrap();
        assert_eq!(row.len(), 6);
    }

    /// `At` is the one layout that fixes *where* rather than how much: one body,
    /// on the mark, measured from the feature's middle.
    #[test]
    fn at_puts_one_body_on_the_mark() {
        let row = offsets(Feature::bounded(8.0, None), Layout::At(2.0), 1, BODY).unwrap();
        assert_eq!(row.len(), 1);
        assert!((row[0] - 2.0).abs() < 1e-12, "{row:?}");
        let centre = offsets(Feature::bounded(8.0, None), Layout::At(0.0), 1, BODY).unwrap();
        assert!(centre[0].abs() < 1e-12);
        // Negative is the other way along the same tangent.
        let back = offsets(Feature::bounded(8.0, None), Layout::At(-3.0), 1, BODY).unwrap();
        assert!((back[0] + 3.0).abs() < 1e-12);
    }

    /// A mark past the end of the host is a refusal, not a clamp — the same
    /// answer every other layout gives, for the same reason.
    #[test]
    fn at_refuses_a_mark_off_the_end() {
        assert!(offsets(Feature::bounded(4.0, None), Layout::At(3.9), 1, BODY).is_err());
        assert!(offsets(Feature::bounded(4.0, None), Layout::At(-3.9), 1, BODY).is_err());
        // Nothing overruns the floor.
        assert!(offsets(Feature::unbounded(), Layout::At(100.0), 1, BODY).is_ok());
    }
}
