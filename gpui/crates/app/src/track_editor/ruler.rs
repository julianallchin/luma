//! The ruler and the grid behind the timeline: beat and bar lines, bar numbers,
//! and the clock ruler an unanalysed track falls back to.

use super::*;

/// The grid behind the timeline, which follows the zoom without a single
/// jump.
///
/// Bars are shaded even and odd, in blocks that run from one bar number to
/// the next, counted from bar 1. Lines are drawn at the finest rung with room,
/// and that rung fades in as the zoom gives it more room. Bar lines are the
/// strongest, beats weaker and beat subdivisions the faintest.
///
/// When the bar numbers halve or double, nothing snaps: the bars that gain or
/// lose a number fade their number, their line weight and the shading edge
/// they carry, over one doubling of the zoom. See [`Labels`].
pub(super) fn paint_beat_grid(
    canvas: Bounds<Pixels>,
    beats: &BeatGrid,
    view: View,
    start: f64,
    end: f64,
    window: &mut Window,
    cx: &mut App,
) {
    let height = f32::from(canvas.size.height);
    let (grid, fade_in) = Grid::at(beats, view.zoom);
    let (beat_length, bar_length) = mean_lengths(beats);
    let labels = Labels::at(bar_length * view.zoom);
    let downbeat_count = beats.downbeats.len();

    // Even and odd blocks, bar by bar, merged into runs of one shade so that
    // two quads never meet mid-block. A bar past the last downbeat ends where
    // the mean bar says it would.
    let bar_at = |index: usize| -> f64 {
        match beats.downbeats.get(index) {
            Some(time) => f64::from(*time),
            None => {
                f64::from(beats.downbeats[downbeat_count - 1])
                    + f64::from(bar_length) * (index + 1 - downbeat_count) as f64
            }
        }
    };
    let first = beats
        .downbeats
        .partition_point(|bar| f64::from(*bar) <= start)
        .saturating_sub(1);
    let mut run: Option<(f64, f32)> = None;
    let shade = |left: f64, right: f64, value: f32, window: &mut Window| {
        if value <= 0. {
            return;
        }
        let (left, right) = (view.x_of(left), view.x_of(right));
        window.paint_quad(fill(
            Bounds {
                origin: point(
                    canvas.origin.x + px(left),
                    canvas.origin.y + px(HEADER_HEIGHT),
                ),
                size: size(px(right - left), px(height - HEADER_HEIGHT)),
            },
            fade(ladder::stripe(), value),
        ));
    };
    for index in first..downbeat_count {
        let left = bar_at(index);
        if left > end {
            break;
        }
        let value = labels.shade(index);
        match run {
            Some((_, shaded)) if shaded == value => {}
            Some((from, shaded)) => {
                shade(from, left, shaded, window);
                run = Some((left, value));
            }
            None => run = Some((left, value)),
        }
    }
    if let Some((from, shaded)) = run {
        let last = beats
            .downbeats
            .partition_point(|bar| f64::from(*bar) <= end)
            .max(first + 1);
        shade(from, bar_at(last), shaded, window);
    }

    // Millisecond-rounded, which is what de-duplicates a downbeat against the
    // beat it sits on. Exact float equality would draw both, a hair apart.
    let downbeats: std::collections::HashSet<i64> = beats
        .downbeats
        .iter()
        .map(|time| (f64::from(*time) * 1000.).round() as i64)
        .collect();

    if let Grid::Beat(divisions) = grid {
        // The ruler's beat ticks fade in between 80 and 120 px/s.
        let tick = ((view.zoom - 80.) / 40.).clamp(0., 1.);
        for (index, beat) in beats.beats.iter().enumerate() {
            let beat = f64::from(*beat);
            let next = beats
                .beats
                .get(index + 1)
                .map_or(beat + f64::from(beat_length), |next| f64::from(*next));
            if next < start {
                continue;
            }
            if beat > end {
                break;
            }
            // Subdivisions: the odd ones belong to the finest rung alone.
            for part in 1..divisions {
                let finest = divisions == 2 || part % 2 == 1;
                let alpha = 0.12 * if finest { fade_in } else { 1. };
                let x = view.x_of(beat + (next - beat) * f64::from(part) / f64::from(divisions));
                window.paint_quad(fill(
                    hairline(canvas, x, HEADER_HEIGHT, height, 1.),
                    fade(ladder::primary(), alpha),
                ));
            }
            if beat < start || downbeats.contains(&((beat * 1000.).round() as i64)) {
                continue;
            }
            let alpha = 0.25 * if divisions == 1 { fade_in } else { 1. };
            let x = view.x_of(beat);
            window.paint_quad(fill(
                hairline(canvas, x, HEADER_HEIGHT, height, 1.),
                fade(ladder::primary(), alpha),
            ));
            if tick > 0. {
                window.paint_quad(fill(
                    hairline(canvas, x, HEADER_HEIGHT - 5., HEADER_HEIGHT, 1.),
                    fade(ladder::primary(), alpha * tick),
                ));
            }
        }
    }

    // A bar number is drawn to the *right* of its downbeat, so a downbeat just
    // left of the viewport still owes it a label.
    let bleed = 40. / f64::from(view.zoom);
    for (index, downbeat) in beats.downbeats.iter().enumerate() {
        let downbeat = f64::from(*downbeat);
        if downbeat < start - bleed || downbeat > end {
            continue;
        }
        let major = labels.weight(index);
        // At a bar rung, only every `n`-th bar has a line, and the odd ones of
        // those belong to the finest rung alone.
        let presence = match grid {
            Grid::Beat(_) => 1.,
            Grid::Bars(n) if index % n as usize != 0 => 0.,
            Grid::Bars(n) if index % (2 * n as usize) != 0 => fade_in,
            Grid::Bars(_) => 1.,
        };
        let alpha = lerp(0.35 * presence, 0.6, major);
        if alpha <= 0. {
            continue;
        }
        let x = view.x_of(downbeat);
        window.paint_quad(fill(
            hairline(
                canvas,
                x,
                HEADER_HEIGHT - lerp(8., 12., major),
                height,
                lerp(1., 2., major),
            ),
            fade(ladder::primary(), alpha),
        ));
        if major > 0. {
            let mut color = ladder::foreground();
            color.a *= major;
            label(
                canvas,
                x + 4.,
                HEADER_HEIGHT - 10.,
                &(index + 1).to_string().into(),
                color,
                window,
                cx,
            );
        }
    }
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

/// Where the bar numbers are, as a smooth function of the zoom.
///
/// `getBarLabelStep` put a number on every `step`-th bar, `step` the power of
/// two that keeps numbers about [`LABEL_SPACING`] apart, and the shading
/// alternates at the same step. Here `level` is `log2` of the exact step
/// that spacing asks for. At a whole level, the numbered bars are the
/// multiples of `2^level`. Between two levels, the multiples of `2^level`
/// that are not multiples of `2^(level + 1)` are on their way out, and
/// `blend` is how far.
struct Labels {
    level: u32,
    blend: f32,
}

/// The gap bar numbers keep, in pixels.
const LABEL_SPACING: f32 = 80.;

impl Labels {
    fn at(bar: f32) -> Self {
        let exact = (LABEL_SPACING / bar.max(f32::EPSILON)).log2().max(0.);
        let level = exact.floor();
        let blend = exact - level;
        Self {
            level: (level as u32).min(30),
            // Smoothstep, so each change eases in and out.
            blend: blend * blend * (3. - 2. * blend),
        }
    }

    /// How much bar `index` (0 is bar 1) is a numbered bar: 1 for a full
    /// number and a heavy line, 0 for a plain bar.
    fn weight(&self, index: usize) -> f32 {
        let rank = if index == 0 {
            u32::MAX
        } else {
            index.trailing_zeros()
        };
        if rank > self.level {
            1.
        } else if rank == self.level {
            1. - self.blend
        } else {
            0.
        }
    }

    /// How shaded bar `index` is, from 0 to 1: the odd blocks of the step
    /// being left, blended into the odd blocks of the step being taken.
    fn shade(&self, index: usize) -> f32 {
        let odd = |level: u32| ((index >> level.min(63)) & 1) as f32;
        lerp(odd(self.level), odd(self.level + 1), self.blend)
    }
}

/// `drawTimeRuler`: the clock ruler an unanalysed track falls back to.
pub(super) fn paint_time_ruler(
    canvas: Bounds<Pixels>,
    view: View,
    start: f64,
    end: f64,
    window: &mut Window,
    cx: &mut App,
) {
    let interval = if view.zoom < 50. { 5 } else { 1 };
    let first = (start / f64::from(interval)).floor() as i64 * i64::from(interval);
    let mut tick = first;
    while (tick as f64) <= end {
        let x = view.x_of(tick as f64);
        let major = tick % 10 == 0;
        window.paint_quad(fill(
            hairline(
                canvas,
                x,
                HEADER_HEIGHT - if major { 10. } else { 5. },
                HEADER_HEIGHT,
                1.,
            ),
            if major {
                ladder::border()
            } else {
                ladder::card()
            },
        ));
        if major {
            label(
                canvas,
                x + 3.,
                HEADER_HEIGHT - 12.,
                &format!("{}:{:02}", tick / 60, tick % 60).into(),
                ladder::muted_foreground(),
                window,
                cx,
            );
        }
        tick += i64::from(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::Labels;

    #[test]
    fn bar_numbers_and_shading_change_without_a_jump() {
        // Bars from 200 px down to 2 px, in small steps: no bar's number,
        // line weight or shade may move more than a little between two.
        let mut previous: Option<Labels> = None;
        let mut bar = 200_f32;
        while bar > 2. {
            let labels = Labels::at(bar);
            if let Some(previous) = &previous {
                for index in 0..256 {
                    assert!((labels.weight(index) - previous.weight(index)).abs() < 0.05);
                    assert!((labels.shade(index) - previous.shade(index)).abs() < 0.05);
                }
            }
            previous = Some(labels);
            bar *= 0.995;
        }
        // At 40 px a bar, every 2nd bar is numbered and blocks are 2 bars.
        let labels = Labels::at(40.);
        assert_eq!(
            [labels.weight(0), labels.weight(1), labels.weight(2)],
            [1., 0., 1.]
        );
        assert_eq!(
            (0..4).map(|index| labels.shade(index)).collect::<Vec<_>>(),
            [0., 0., 1., 1.]
        );
    }
}
