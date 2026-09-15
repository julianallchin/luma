//! Annotation Preview Generator
//!
//! Generates space-time heatmap thumbnails for timeline annotations.
//! Each preview is a small RGBA image where rows = fixtures, columns = time steps,
//! and pixel color = fixture RGB × dimmer.

use crate::models::node_graph::BeatGrid;
use crate::models::patterns::AnnotationPreview;
use crate::models::universe::UniverseState;

const MAX_PREVIEW_HEIGHT: u32 = 32;

fn empty_preview(annotation_id: String) -> AnnotationPreview {
    AnnotationPreview {
        annotation_id,
        width: 1,
        height: 1,
        pixels: vec![0, 0, 0, 0],
        dominant_color: [0.0; 3],
    }
}

/// Render a heatmap from a column-sampled grid of [`UniverseState`] frames
/// (`frames[col]` = the state at preview column `col`). Rows are primitives,
/// ordered by brightness-weighted center of mass in time so spatial patterns
/// read as diagonals; past [`MAX_PREVIEW_HEIGHT`] heads, a row is the
/// brightest head of a band of neighbours in that order.
pub(crate) fn render_preview(
    annotation_id: String,
    frames: &[UniverseState],
    beat_grid: Option<&BeatGrid>,
    start_time: f32,
    end_time: f32,
) -> AnnotationPreview {
    let _ = (beat_grid, start_time, end_time); // width is implied by frames.len()
    let width = frames.len() as u32;
    if width == 0 {
        return empty_preview(annotation_id);
    }

    // Collect the set of primitive ids across all columns.
    let mut prim_ids: Vec<String> = {
        let mut set: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for f in frames {
            for k in f.primitives.keys() {
                set.insert(k.as_str());
            }
        }
        set.into_iter().map(|s| s.to_string()).collect()
    };
    if prim_ids.is_empty() {
        return empty_preview(annotation_id);
    }

    // Brightness-weighted center of mass per primitive.
    prim_ids.sort_by(|a, b| {
        let com = |id: &str| -> f64 {
            let mut weighted = 0.0f64;
            let mut total = 0.0f64;
            for (col, f) in frames.iter().enumerate() {
                let d = f.primitives.get(id).map(|p| p.dimmer).unwrap_or(0.0) as f64;
                weighted += col as f64 * d;
                total += d;
            }
            if total > 0.0 {
                weighted / total
            } else {
                f64::MAX
            }
        };
        com(a)
            .partial_cmp(&com(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let height = (prim_ids.len() as u32).min(MAX_PREVIEW_HEIGHT);
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let mut color_sum = [0.0f64; 3];
    let mut weight_sum = 0.0f64;

    // A rig with more heads than rows shares each row between a band of
    // neighbours in that order and shows the brightest of them, so no head is
    // dropped from the strip.
    let band = |row: usize| {
        let (n, h) = (prim_ids.len(), height as usize);
        &prim_ids[row * n / h..(row + 1) * n / h]
    };
    for row in 0..height as usize {
        for col in 0..width as usize {
            let frame = &frames[col];
            let (color, dimmer) = band(row)
                .iter()
                .filter_map(|id| frame.primitives.get(id).map(|p| (p.color, p.dimmer)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or(([1.0, 1.0, 1.0], 0.0));

            let r = (color[0] * dimmer * 255.0).clamp(0.0, 255.0) as u8;
            let g = (color[1] * dimmer * 255.0).clamp(0.0, 255.0) as u8;
            let b = (color[2] * dimmer * 255.0).clamp(0.0, 255.0) as u8;

            let idx = ((row as u32 * width + col as u32) * 4) as usize;
            pixels[idx] = r;
            pixels[idx + 1] = g;
            pixels[idx + 2] = b;
            pixels[idx + 3] = 255;

            color_sum[0] += r as f64;
            color_sum[1] += g as f64;
            color_sum[2] += b as f64;
            weight_sum += 1.0;
        }
    }

    let dominant_color = if weight_sum > 0.0 {
        [
            (color_sum[0] / weight_sum / 255.0) as f32,
            (color_sum[1] / weight_sum / 255.0) as f32,
            (color_sum[2] / weight_sum / 255.0) as f32,
        ]
    } else {
        [0.0; 3]
    };

    AnnotationPreview {
        annotation_id,
        width,
        height,
        pixels,
        dominant_color,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::models::universe::PrimitiveState;

    fn frame(lit: impl Iterator<Item = usize>) -> UniverseState {
        let mut primitives = HashMap::new();
        for head in 0..64 {
            primitives.insert(
                format!("head-{head:02}"),
                PrimitiveState {
                    dimmer: 0.0,
                    color: [0.0, 1.0, 0.0],
                    strobe: 0.0,
                    position: [0.0; 2],
                    speed: 1.0,
                },
            );
        }
        for head in lit {
            primitives
                .get_mut(&format!("head-{head:02}"))
                .unwrap()
                .dimmer = 1.0;
        }
        UniverseState { primitives }
    }

    #[test]
    fn every_head_reaches_the_strip_when_there_are_more_heads_than_rows() {
        // Half the rig lights only at the start, the other half only at the
        // end. A strip that kept the 32 earliest heads would go dark after
        // the first column.
        let mut frames = vec![frame(0..32)];
        frames.extend((0..30).map(|_| frame(std::iter::empty())));
        frames.push(frame(32..64));
        let preview = render_preview("clip".into(), &frames, None, 0.0, 1.0);
        assert_eq!((preview.width, preview.height), (32, MAX_PREVIEW_HEIGHT));
        let green_at = |col: u32| {
            (0..preview.height)
                .map(|row| preview.pixels[((row * preview.width + col) * 4 + 1) as usize])
                .max()
                .unwrap()
        };
        assert_eq!(green_at(0), 255);
        assert_eq!(green_at(15), 0);
        assert_eq!(green_at(31), 255);
    }
}
