use super::{clear_region, Clip};
use gpui::SharedString;

/// One visible row per lighting layer, highest z at the top. Row 0 is the
/// empty insertion lane above them.
pub(super) fn assign_rows(clips: &mut [Clip]) {
    let mut zs: Vec<i64> = clips.iter().map(|clip| clip.z).collect();
    zs.sort_unstable_by(|a, b| b.cmp(a));
    zs.dedup();
    for clip in clips {
        clip.row = 1 + zs.partition_point(|&z| z > clip.z);
    }
}

/// How far one clip may run into another and still count as only touching
/// it. Seconds are derived from stored beats, and that round trip is not bit
/// exact.
const TOUCH: f64 = 1e-6;

/// The clips after the user placed `placed`: each one cuts away the part of
/// every other clip in its layer that it covers, so layers never overlap.
/// Placed clips are taken in time order, so where two of them overlap the
/// earlier one wins. `kept` clips are not cut. `None` when nothing was cut.
pub(super) fn settle(
    clips: &[Clip],
    placed: &[SharedString],
    kept: &[SharedString],
) -> Option<Vec<Clip>> {
    let mut order: Vec<&Clip> = clips
        .iter()
        .filter(|clip| placed.contains(&clip.id))
        .collect();
    order.sort_by(|a, b| a.start.total_cmp(&b.start));
    let order: Vec<SharedString> = order.into_iter().map(|clip| clip.id.clone()).collect();
    let mut out = clips.to_vec();
    let mut changed = false;
    for (index, id) in order.iter().enumerate() {
        // An earlier placed clip may have covered this one whole.
        let Some(clip) = out.iter().find(|clip| &clip.id == id) else {
            continue;
        };
        let (z, from, to) = (clip.z, clip.start, clip.end);
        let done = &order[..=index];
        let cut = |other: &Clip| {
            other.z == z
                && other.start < to - TOUCH
                && from < other.end - TOUCH
                && !done.contains(&other.id)
                && !kept.contains(&other.id)
        };
        if out.iter().any(cut) {
            out = clear_region(&out, (from, to), cut);
            changed = true;
        }
    }
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track_editor::BlendMode;

    fn clip(id: &str, start: f64, end: f64, z: i64) -> Clip {
        Clip {
            id: id.to_owned().into(),
            output: "color".into(),
            label: id.to_owned().into(),
            summary: gpui::SharedString::default(),
            color: luma_ui::ladder::pattern("test"),
            start,
            end,
            row: 0,
            z,
            blend: BlendMode::Replace,
            core: None,
        }
    }

    fn spans(clips: &[Clip]) -> Vec<(&str, f64, f64, i64)> {
        let mut spans: Vec<_> = clips
            .iter()
            .map(|c| (c.id.as_ref(), c.start, c.end, c.z))
            .collect();
        spans.sort_by(|a, b| a.3.cmp(&b.3).then(a.1.total_cmp(&b.1)));
        spans
    }

    #[test]
    fn each_layer_is_one_row_below_the_insertion_lane() {
        let mut clips = vec![
            clip("bed", 0., 10., 0),
            clip("accent", 1., 3., 4),
            clip("next", 3., 5., 4),
            clip("top", 0., 10., 7),
        ];
        assign_rows(&mut clips);
        let rows: Vec<usize> = clips.iter().map(|c| c.row).collect();
        assert_eq!(rows, vec![3, 2, 2, 1]);
    }

    #[test]
    fn a_placed_clip_trims_splits_and_removes_what_it_covers() {
        let clips = vec![
            clip("placed", 2., 6., 0),
            clip("before", 0., 3., 0),
            clip("around", 1., 8., 0),
            clip("inside", 3., 4., 0),
            clip("other layer", 2., 6., 1),
        ];
        let settled = settle(&clips, &["placed".into()], &[]).unwrap();
        let mut spans = spans(&settled);
        // The far piece of the split clip is a copy with a new id.
        let tail = spans.iter().position(|s| s.1 == 6.).unwrap();
        assert!(spans[tail].0.starts_with("new:"));
        spans.remove(tail);
        assert_eq!(
            spans,
            vec![
                ("before", 0., 2., 0),
                ("around", 1., 2., 0),
                ("placed", 2., 6., 0),
                ("other layer", 2., 6., 1),
            ]
        );
    }

    #[test]
    fn a_clip_that_only_touches_another_cuts_nothing() {
        // The end comes back from a beat round trip a hair past the start.
        let clips = vec![clip("a", 0., 2.000_000_000_001, 0), clip("b", 2., 4., 0)];
        assert!(settle(&clips, &["b".into()], &[]).is_none());
        assert!(settle(&clips, &["a".into()], &[]).is_none());
    }

    #[test]
    fn kept_clips_are_not_cut() {
        let clips = vec![clip("a", 0., 4., 0), clip("b", 2., 6., 0)];
        assert!(settle(&clips, &["b".into()], &["a".into()]).is_none());
    }

    #[test]
    fn the_earlier_of_two_placed_clips_wins() {
        let clips = vec![clip("a", 0., 4., 0), clip("b", 2., 6., 0)];
        let settled = settle(&clips, &["b".into(), "a".into()], &[]).unwrap();
        assert_eq!(spans(&settled), vec![("a", 0., 4., 0), ("b", 4., 6., 0)]);
    }
}
