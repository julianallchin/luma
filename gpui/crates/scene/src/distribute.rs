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
//! - [`Layout::Span`] narrows the segment to a fraction of the feature and is
//!   then [`Layout::Even`] within it.
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
    /// Evenly across the fraction `t0..t1` of the feature, `0` at the feature's
    /// negative-tangent end. Reversed or out-of-range fractions are ordered and
    /// clamped rather than refused — a dragged handle produces both, and
    /// neither is a mistake anybody can act on.
    Span(f64, f64),
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
    let (segment_start, segment_length) = match feature.length_m {
        Some(length) => segment(length, layout),
        None => (-band / 2.0, band),
    };
    if band > segment_length + FIT_EPSILON_M {
        return Err(too_long(feature, layout, band));
    }

    let pitch = match layout {
        Layout::Spacing(spacing) => spacing.abs(),
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
    let first = segment_start + (segment_length - extent) / 2.0 + width / 2.0;
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
        Layout::Span(t0, t1) => {
            let (lo, hi) = fractions(t0, t1);
            (lo * length - length / 2.0, (hi - lo) * length)
        }
    }
}

/// A span's two fractions, ordered and clamped into `0..=1`.
fn fractions(t0: f64, t1: f64) -> (f64, f64) {
    let clamp = |t: f64| {
        if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let (a, b) = (clamp(t0), clamp(t1));
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
        Layout::Even | Layout::Span(_, _) => count as f64 * width,
        Layout::Spacing(spacing) => (count - 1) as f64 * spacing.abs() + width,
    }
}

/// The refusal: how long the whole feature would have to be for this band.
///
/// A `Span` distribution asked for a fraction of the feature, so its need is
/// scaled back up by that fraction — extending a truss to the band's length
/// would not help when the caller only offered it a quarter of the truss.
fn too_long(feature: Feature, layout: Layout, band: f64) -> Fit {
    let available = feature.length_m.unwrap_or(f64::INFINITY);
    let whole = match layout {
        Layout::Even | Layout::Spacing(_) => band,
        Layout::Span(t0, t1) => {
            let (lo, hi) = fractions(t0, t1);
            if hi - lo > 0.0 {
                band / (hi - lo)
            } else {
                f64::INFINITY
            }
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
        for layout in [Layout::Even, Layout::Spacing(0.4), Layout::Span(0.0, 1.0)] {
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
        for layout in [Layout::Even, Layout::Spacing(0.6), Layout::Span(0.0, 1.0)] {
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
    fn a_span_lays_out_inside_its_fraction() {
        let row = spread(4.0, Layout::Span(0.5, 1.0), 3).unwrap();
        let midpoint = (row[0] + row[2]) / 2.0;
        assert!((midpoint - 1.0).abs() < 1e-9, "{row:?}");
        assert!(row[0] >= 0.0 - 1e-9 && row[2] <= 2.0 + 1e-9, "{row:?}");
    }

    #[test]
    fn a_reversed_span_is_the_span_it_names() {
        assert_eq!(
            spread(4.0, Layout::Span(1.0, 0.5), 3).unwrap(),
            spread(4.0, Layout::Span(0.5, 1.0), 3).unwrap()
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
        for layout in [Layout::Even, Layout::Spacing(0.7), Layout::Span(0.25, 0.75)] {
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
    fn a_spans_need_is_scaled_back_to_the_whole_feature() {
        let Err(Fit::TooLong { needed_m, .. }) = offsets(
            Feature::bounded(2.0, None),
            Layout::Span(0.0, 0.25),
            4,
            0.25,
        ) else {
            panic!("four 0.25 m bodies do not fit in half a metre");
        };
        assert!((needed_m - 4.0).abs() < 1e-9, "{needed_m}");
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
}
