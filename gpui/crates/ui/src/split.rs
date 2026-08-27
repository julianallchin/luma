//! How a region divides its space between two neighbours.
//!
//! [`crate::pane`]'s sibling, and deliberately a smaller thing: a pane
//! *animates* between two authored sizes, while a split is dragged and a drag
//! is already continuous. There is no tween here and nothing to ask a `Window`
//! for.
//!
//! Axis-free on purpose. The shell divides a row between its thread and its
//! workspace panel with this, and the workspace column divides its height
//! between the stage and the editor with it — the arithmetic of "two regions
//! sharing a span, each with a floor" does not know which way it points, and
//! two copies that did would be the same rule written twice.
//!
//! # The fraction is stored, the pixels are derived
//!
//! What survives a resize is the *proportion*, not either height: a window
//! dragged taller should grow both regions, and a stored pixel height would
//! instead grow one and pin the other. Every height in the interface is
//! therefore resolved from the fraction against this frame's available space.
//!
//! # Minimums are a property of the split, not of its callers
//!
//! A caller that clamped its own heights would have to restate both minimums at
//! every read — the render, the drag, and any arithmetic that sizes a sibling —
//! and three statements of one rule are three chances to disagree. [`resolve`]
//! is total: it answers for any available height, including one too small to
//! honour both minimums, so there is no error for a caller to handle.
//!
//! [`resolve`]: SplitFraction::resolve

/// A region's division between its leading and trailing neighbour — left/right
/// in a row, top/bottom in a column.
///
/// Held by whatever owns the two regions, since it outlives the elements that
/// render them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplitFraction {
    /// Share of the available space the leading region rests at, before
    /// minimums.
    fraction: f32,
    /// Where a double-click on the seam sends it back to.
    default_fraction: f32,
    min_leading: f32,
    min_trailing: f32,
}

impl SplitFraction {
    /// A split resting at `fraction` of the available space, where neither
    /// region may be dragged below its minimum.
    ///
    /// # Panics
    ///
    /// Never. `fraction` is clamped to `0.0..=1.0` and the minimums are floored
    /// at zero, so a nonsensical authored value is a usable split rather than a
    /// deferred panic in the first frame that renders it.
    #[must_use]
    pub fn new(fraction: f32, min_leading: f32, min_trailing: f32) -> Self {
        let fraction = fraction.clamp(0.0, 1.0);
        Self {
            fraction,
            default_fraction: fraction,
            min_leading: min_leading.max(0.0),
            min_trailing: min_trailing.max(0.0),
        }
    }

    /// The stored proportion, before this frame's minimums are applied. Callers
    /// that want sizes want [`Self::resolve`]; this is for persisting the
    /// split, which is a proportion and not a pixel count.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        self.fraction
    }

    /// This frame's `(leading, trailing)` sizes for `available` space.
    ///
    /// Total by construction. When `available` cannot seat both minimums the
    /// two regions divide it by the stored proportion instead — a window too
    /// small for the authored floors still shows both regions rather than
    /// letting one overflow the region.
    #[must_use]
    pub fn resolve(&self, available: f32) -> (f32, f32) {
        let available = available.max(0.0);
        if available <= self.min_leading + self.min_trailing {
            let leading = available * self.fraction;
            return (leading, available - leading);
        }
        let leading =
            (available * self.fraction).clamp(self.min_leading, available - self.min_trailing);
        (leading, available - leading)
    }

    /// Send the seam to `offset` pixels along the region, measured from its
    /// leading edge.
    ///
    /// The offset is clamped through [`Self::resolve`]'s own bounds, so a drag
    /// that runs past either end parks the seam at the minimum rather than
    /// storing a fraction the next render would have to clamp again.
    pub fn drag_to(&mut self, offset: f32, available: f32) {
        if available <= 0.0 {
            return;
        }
        let lowest = self.min_leading.min(available);
        let highest = (available - self.min_trailing).max(lowest);
        self.fraction = offset.clamp(lowest, highest) / available;
    }

    /// Restore the authored split — the gesture a seam answers to on a
    /// double-click.
    pub fn reset(&mut self) {
        self.fraction = self.default_fraction;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split() -> SplitFraction {
        SplitFraction::new(0.4, 120.0, 200.0)
    }

    #[test]
    fn the_two_regions_always_fill_exactly_the_available_space() {
        for available in [0.0, 10.0, 319.0, 320.0, 1000.0] {
            let (leading, trailing) = split().resolve(available);
            assert!(
                (leading + trailing - available).abs() < 0.001,
                "at {available}"
            );
            assert!(leading >= 0.0 && trailing >= 0.0, "at {available}");
        }
    }

    #[test]
    fn the_proportion_is_what_survives_a_resize() {
        let (leading, _) = split().resolve(1000.0);
        assert!((leading - 400.0).abs() < 0.001);
        let (leading, _) = split().resolve(2000.0);
        assert!((leading - 800.0).abs() < 0.001);
    }

    #[test]
    fn minimums_win_over_the_proportion() {
        // 40% of 400 is 160, above the 120 floor: the proportion stands.
        let (leading, trailing) = split().resolve(400.0);
        assert!((leading - 160.0).abs() < 0.001);
        assert!((trailing - 240.0).abs() < 0.001);
        // 40% of 330 is 132, which would leave the trailing region below its
        // 200 floor.
        let (leading, trailing) = split().resolve(330.0);
        assert!((leading - 130.0).abs() < 0.001);
        assert!((trailing - 200.0).abs() < 0.001);
    }

    #[test]
    fn a_region_too_small_for_both_floors_still_shows_both_neighbours() {
        let (leading, trailing) = split().resolve(200.0);
        assert!(leading > 0.0 && trailing > 0.0);
        assert!((leading + trailing - 200.0).abs() < 0.001);
    }

    #[test]
    fn dragging_past_an_end_parks_at_that_minimum() {
        let mut split = split();
        split.drag_to(-500.0, 1000.0);
        assert!((split.resolve(1000.0).0 - 120.0).abs() < 0.001);
        split.drag_to(5000.0, 1000.0);
        assert!((split.resolve(1000.0).0 - 800.0).abs() < 0.001);
    }

    #[test]
    fn a_drag_lands_where_the_pointer_is() {
        let mut split = split();
        split.drag_to(650.0, 1000.0);
        assert!((split.resolve(1000.0).0 - 650.0).abs() < 0.001);
    }

    #[test]
    fn dragging_a_collapsed_region_is_a_no_op_rather_than_a_division_by_zero() {
        let mut split = split();
        split.drag_to(100.0, 0.0);
        assert!(split.fraction().is_finite());
        assert_eq!(split.fraction(), 0.4);
    }

    #[test]
    fn double_click_restores_the_authored_split() {
        let mut split = split();
        split.drag_to(900.0, 1000.0);
        assert_ne!(split.fraction(), 0.4);
        split.reset();
        assert_eq!(split.fraction(), 0.4);
    }

    #[test]
    fn an_authored_fraction_out_of_range_is_clamped_rather_than_trusted() {
        assert_eq!(SplitFraction::new(-1.0, 0.0, 0.0).fraction(), 0.0);
        assert_eq!(SplitFraction::new(9.0, 0.0, 0.0).fraction(), 1.0);
    }
}
