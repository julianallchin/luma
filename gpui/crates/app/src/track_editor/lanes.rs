use super::Clip;
use std::collections::BTreeMap;

/// Crossing an overflow row at the same priority still means moving a layer.
/// Skip those presentation rows so a vertical drag cannot silently snap back.
pub(super) fn drag_z(layers: &[i64], from: i32, delta: i32) -> i64 {
    let current = super::row_to_z(layers, from);
    let mut target = from + delta;
    if delta != 0 {
        while layers.get(target as usize) == Some(&current) {
            target += delta.signum();
        }
        if delta > 0 && target >= layers.len() as i32 {
            return current;
        }
    }
    super::row_to_z(layers, target)
}

/// Pack each lighting layer into enough visible rows to expose every clip.
/// This is presentation only: z, blend order, IDs and score contents stay intact.
/// Half-open spans let consecutive clips reuse a row at a shared boundary.
pub(super) fn assign_rows(clips: &mut [Clip]) {
    let mut layers: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (index, clip) in clips.iter().enumerate() {
        layers.entry(clip.z).or_default().push(index);
    }
    let mut count = 0;
    for indices in layers.values_mut() {
        indices.sort_by(|&a, &b| {
            clips[a]
                .start
                .total_cmp(&clips[b].start)
                .then_with(|| clips[a].id.cmp(&clips[b].id))
        });
        let mut ends: Vec<f64> = Vec::new();
        for &index in indices.iter() {
            let clip = &mut clips[index];
            let slot = ends
                .iter()
                .position(|end| *end <= clip.start)
                .unwrap_or(ends.len());
            if slot == ends.len() {
                ends.push(clip.end);
            } else {
                ends[slot] = clip.end;
            }
            clip.row = count + slot;
        }
        count += ends.len();
    }
    for clip in clips {
        clip.row = count - clip.row;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track_editor::{clear_region, BlendMode};

    #[test]
    fn dragging_across_an_overflow_row_changes_the_actual_layer() {
        assert_eq!(drag_z(&[1, 0, 0], 2, -1), 1);
        assert_eq!(drag_z(&[1, 0, 0], 2, 0), 0);
        assert_eq!(drag_z(&[1, 1, 0], 1, -1), 2);
        assert_eq!(drag_z(&[1, 0, 0], 1, 1), 0);
    }

    fn clip(id: &str, start: f64, end: f64, z: i64) -> Clip {
        Clip {
            id: id.to_owned().into(),
            pattern: "test".into(),
            label: id.to_owned().into(),
            color: luma_ui::ladder::pattern("test"),
            start,
            end,
            row: 0,
            z,
            blend: BlendMode::Replace,
            args: serde_json::json!({}),
            core: None,
        }
    }

    #[test]
    fn overlaps_are_accessible_without_changing_lighting_priority() {
        let mut clips = vec![
            clip("bed", 0., 10., 0),
            clip("accent", 1., 3., 0),
            clip("next", 3., 5., 0),
            clip("top", 0., 10., 1),
        ];
        assign_rows(&mut clips);
        assert_ne!(clips[0].row, clips[1].row);
        assert_eq!(clips[1].row, clips[2].row);
        assert!(clips[3].row < clips[1].row);
        assert_eq!(
            clips.iter().map(|c| c.z).collect::<Vec<_>>(),
            vec![0, 0, 0, 1]
        );
        // Deleting a visible row must not also trim a hidden peer at the same z.
        let row = clips[1].row;
        let edited = clear_region(&clips, (1., 3.), row..=row);
        assert!(!edited.iter().any(|c| c.id == "accent"));
        let bed = edited.iter().find(|c| c.id == "bed").unwrap();
        assert_eq!((bed.start, bed.end), (0., 10.));
    }

    #[test]
    fn layout_is_independent_of_input_order_and_does_not_accumulate_rows() {
        let mut clips: Vec<_> = (0..100)
            .map(|i| clip(&format!("clip-{i}"), i as f64, i as f64 + 2., 0))
            .collect();
        assign_rows(&mut clips);
        let expected: BTreeMap<_, _> = clips.iter().map(|c| (c.id.clone(), c.row)).collect();
        assert_eq!(clips.iter().map(|c| c.row).max(), Some(2));
        clips.reverse();
        assign_rows(&mut clips);
        assert!(clips.iter().all(|c| expected[&c.id] == c.row));
    }
}
