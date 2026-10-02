//! The beat grid: a time node's constant `every`, `duration` and `delay` are
//! a multiple of 1/32 or of 1/24 of a beat (spec: `docs/specs/clip-graphs.md`,
//! the time row in section 0). A wired value is free.
use super::Kind;

/// The smallest `every` and `duration`: a 32nd of a beat.
pub const SMALLEST_BEAT: f64 = 1. / 32.;

/// How far from the grid a value may be and still be on it, in beats.
const TOLERANCE: f64 = 1e-9;

/// The denominators of the grid, smallest first: a fraction names the first
/// that fits.
const DENOMINATORS: [f64; 10] = [1., 2., 3., 4., 6., 8., 12., 16., 24., 32.];

/// Whether a constant in `input` of a `kind` node must lie on the grid.
#[must_use]
pub fn on_grid(kind: Kind, input: &str) -> bool {
    kind == Kind::Time && matches!(input, "every" | "duration" | "delay")
}

/// The grid value nearest `beats` for `input` of a `kind` node: the nearer of
/// the nearest 32nd and the nearest 24th (a tie takes the 32nd). `every` and
/// `duration` are at least [`SMALLEST_BEAT`]. Any other input, or a value that
/// is not finite, is unchanged.
#[must_use]
pub fn snap(kind: Kind, input: &str, beats: f64) -> f64 {
    if !on_grid(kind, input) || !beats.is_finite() {
        return beats;
    }
    let straight = (beats * 32.).round() / 32.;
    let triplet = (beats * 24.).round() / 24.;
    let nearest = if (triplet - beats).abs() < (straight - beats).abs() {
        triplet
    } else {
        straight
    };
    if input == "delay" {
        nearest
    } else {
        nearest.max(SMALLEST_BEAT)
    }
}

/// Whether `beats` in `input` of a `kind` node is on the grid, or needs none.
#[must_use]
pub fn is_on_grid(kind: Kind, input: &str, beats: f64) -> bool {
    (snap(kind, input, beats) - beats).abs() <= TOLERANCE
}

/// A grid value as the fraction with the smallest denominator: `1/8`, `4/3`,
/// `2`. A value off the grid shows as its digits.
#[must_use]
pub fn fraction(beats: f64) -> String {
    DENOMINATORS
        .iter()
        .find_map(|&d| {
            let top = beats * d;
            // + 0. turns -0 into 0.
            let whole = top.round() + 0.;
            ((top - whole).abs() <= TOLERANCE * d).then(|| {
                if d == 1. {
                    format!("{whole}")
                } else {
                    format!("{whole}/{d}")
                }
            })
        })
        .unwrap_or_else(|| format!("{beats}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_the_nearer_of_a_32nd_and_a_24th() {
        let every = |v| snap(Kind::Time, "every", v);
        assert_eq!(every(0.1), 3. / 32.);
        assert_eq!(every(0.3333), 1. / 3.);
        assert_eq!(every(8.000075), 8.);
        assert_eq!(every(0.500169), 0.5);
        assert_eq!(every(0.0386847), 1. / 24.);
        assert_eq!(every(0.), SMALLEST_BEAT);
        assert_eq!(every(-1.), SMALLEST_BEAT);
        assert_eq!(snap(Kind::Time, "delay", -0.01), 0.);
        assert_eq!(snap(Kind::Time, "delay", -0.3), -7. / 24.);
        assert_eq!(snap(Kind::Time, "phase", 0.1), 0.1);
        assert_eq!(snap(Kind::Space, "every", 0.1), 0.1);
    }

    #[test]
    fn names_the_smallest_denominator() {
        assert_eq!(fraction(0.125), "1/8");
        assert_eq!(fraction(1. / 3.), "1/3");
        assert_eq!(fraction(2. / 24.), "1/12");
        assert_eq!(fraction(29. / 32.), "29/32");
        assert_eq!(fraction(8.), "8");
        assert_eq!(fraction(-0.5), "-1/2");
        assert_eq!(fraction(0.), "0");
    }
}
