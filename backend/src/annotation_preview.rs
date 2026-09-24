//! Annotation Preview Generator
//!
//! Generates space-time heatmap thumbnails for timeline annotations.
//! Each preview is a small RGBA image where rows = fixtures, columns = time steps,
//! and pixel color = fixture RGB × dimmer.

use std::collections::HashMap;

use crate::models::patterns::AnnotationPreview;
use crate::models::universe::UniverseState;
use luma_patterns as p;

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

/// Where each head of a form clip sits along the line its strip is sorted
/// by: the clip's own `axis` when it has one, resolved the way the engine
/// resolves it, else the selection's major axis. Heads the line cannot place
/// keep their selection order.
pub(crate) fn head_order(clip: &p::Clip, cells: &[p::Cell]) -> HashMap<String, f64> {
    let major = p::MappingSpec {
        span: Default::default(),
        plane: None,
        source: p::MappingSource::MajorAxis {
            toward: [0., 0., 1.],
        },
        per_group: false,
        reverse: false,
        mirror: None,
    };
    let axis = match clip.inputs.get("axis") {
        Some(p::Value::Mapping(spec)) => spec.resolve(cells).ok(),
        _ => None,
    };
    match axis.or_else(|| major.resolve(cells).ok()) {
        Some(mapping) => mapping
            .coordinates
            .into_iter()
            .map(|coordinate| (coordinate.cell, coordinate.position))
            .collect(),
        None => cells
            .iter()
            .enumerate()
            .map(|(index, cell)| (cell.id.clone(), index as f64))
            .collect(),
    }
}

/// Render a heatmap from a column-sampled grid of [`UniverseState`] frames
/// (`frames[col]` = the state at preview column `col`). Rows are primitives.
///
/// With an `order` (see [`head_order`]) rows follow it, and past
/// [`MAX_PREVIEW_HEIGHT`] heads a row is the mean of a band of neighbours:
/// what that stretch of the rig shows. Without one, rows are ordered by
/// brightness-weighted center of mass in time so spatial patterns read as
/// diagonals, and a row is the brightest head of its band.
pub(crate) fn render_preview(
    annotation_id: String,
    frames: &[UniverseState],
    order: Option<&HashMap<String, f64>>,
) -> AnnotationPreview {
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

    if let Some(order) = order {
        let at = |id: &str| order.get(id).copied().unwrap_or(f64::MAX);
        prim_ids.sort_by(|a, b| at(a).total_cmp(&at(b)).then_with(|| a.cmp(b)));
    } else {
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
    }

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
            let heads = band(row).iter().filter_map(|id| frame.primitives.get(id));
            let shown = |head: &crate::models::universe::PrimitiveState| {
                head.color.map(|channel| channel * head.dimmer)
            };
            let rgb = if order.is_some() {
                let (sum, count) = heads.fold(([0.0f32; 3], 0usize), |(sum, count), head| {
                    let rgb = shown(head);
                    (std::array::from_fn(|i| sum[i] + rgb[i]), count + 1)
                });
                sum.map(|channel| channel / count.max(1) as f32)
            } else {
                heads
                    .max_by(|a, b| a.dimmer.total_cmp(&b.dimmer))
                    .map_or([0.0; 3], shown)
            };

            let r = (rgb[0] * 255.0).clamp(0.0, 255.0) as u8;
            let g = (rgb[1] * 255.0).clamp(0.0, 255.0) as u8;
            let b = (rgb[2] * 255.0).clamp(0.0, 255.0) as u8;

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

/// The size of an aim clip's picture: the shape of a preset tile's strip.
const AIM_PREVIEW: (u32, u32) = (128, 28);
/// Where in the clip an aim picture shows the beams, as a share of it.
const AIM_MOMENT: f64 = 0.375;

/// An aim clip's picture. An aim has no light for a strip to show, so this
/// is the rig seen from above, the audience at the bottom. Each beam is
/// drawn a fixed length from its head: bright where it points at one moment
/// of the clip, and dim along the path its tip takes over the whole clip. A
/// fan, a wave and a circle each keep their shapes. `frames` are samples
/// across the clip, in order.
pub(crate) fn render_aim_preview(
    annotation_id: String,
    frames: &[UniverseState],
    cells: &[p::Cell],
) -> AnnotationPreview {
    let (width, height) = AIM_PREVIEW;
    let heads: Vec<[f64; 2]> = cells
        .iter()
        .map(|cell| [cell.uvz[0], cell.uvz[1]])
        .collect();
    let spread = |axis: usize| {
        let values = heads.iter().map(|at| at[axis]);
        values.clone().fold(f64::MIN, f64::max) - values.fold(f64::MAX, f64::min)
    };
    let beam = 0.15 * spread(0).max(spread(1)).max(0.5);
    // Each head's beam tip at every frame, in metres; `None` with no aim.
    let tips: Vec<Vec<Option<[f64; 2]>>> = cells
        .iter()
        .zip(&heads)
        .map(|(cell, at)| {
            frames
                .iter()
                .map(|frame| {
                    let [du, dv, _] = frame.primitives.get(&cell.id)?.aim?.direction;
                    Some([at[0] + f64::from(du) * beam, at[1] + f64::from(dv) * beam])
                })
                .collect()
        })
        .collect();
    let drawn: Vec<[f64; 2]> = heads
        .iter()
        .copied()
        .chain(tips.iter().flatten().flatten().copied())
        .collect();
    if drawn.is_empty() {
        return empty_preview(annotation_id);
    }
    let low = |axis: usize| drawn.iter().map(|at| at[axis]).fold(f64::MAX, f64::min);
    let high = |axis: usize| drawn.iter().map(|at| at[axis]).fold(f64::MIN, f64::max);
    // One scale for both axes, so shapes are not squashed; 2 px of air.
    let scale = ((f64::from(width) - 4.) / (high(0) - low(0)).max(1e-6))
        .min((f64::from(height) - 4.) / (high(1) - low(1)).max(1e-6));
    let middle = [(low(0) + high(0)) / 2., (low(1) + high(1)) / 2.];
    let pixel = |at: [f64; 2]| {
        [
            f64::from(width) / 2. + (at[0] - middle[0]) * scale,
            f64::from(height) / 2. + (at[1] - middle[1]) * scale,
        ]
    };
    let mut level = vec![0f32; (width * height) as usize];
    let plot = |level: &mut [f32], [x, y]: [f64; 2], value: f32| {
        let (col, row) = (x.floor(), y.floor());
        if col >= 0. && row >= 0. && col < f64::from(width) && row < f64::from(height) {
            let at = (row as u32 * width + col as u32) as usize;
            level[at] = level[at].max(value);
        }
    };
    let line = |level: &mut [f32], a: [f64; 2], b: [f64; 2], value: f32| {
        let (a, b) = (pixel(a), pixel(b));
        let steps = (b[0] - a[0]).abs().max((b[1] - a[1]).abs()).ceil().max(1.);
        for step in 0..=steps as usize {
            let t = step as f64 / steps;
            plot(
                level,
                [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t],
                value,
            );
        }
    };
    let moment = ((frames.len().saturating_sub(1)) as f64 * AIM_MOMENT).round() as usize;
    for (head, path) in heads.iter().zip(&tips) {
        for pair in path.windows(2) {
            if let [Some(a), Some(b)] = pair {
                line(&mut level, *a, *b, 0.35);
            }
        }
        if let Some(Some(tip)) = path.get(moment) {
            line(&mut level, *head, *tip, 1.);
        }
        plot(&mut level, pixel(*head), 0.5);
    }
    let mean = level.iter().sum::<f32>() / level.len() as f32;
    let pixels = level
        .iter()
        .flat_map(|value| {
            let v = (value * 255.).round() as u8;
            [v, v, v, 255]
        })
        .collect();
    AnnotationPreview {
        annotation_id,
        width,
        height,
        pixels,
        dominant_color: [mean; 3],
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
                    aim: None,
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
        let preview = render_preview("clip".into(), &frames, None);
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
