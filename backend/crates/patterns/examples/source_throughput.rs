//! Warm tensor evaluation cost. Excludes preparation, JSON, UI and GPU rendering.
use luma_patterns::*;
use std::{hint::black_box, time::Instant};
fn main() -> Result<()> {
    let library = standard_library();
    for count in [120, 480, 960] {
        let cells: Vec<_> = (0..count)
            .map(|n| Cell {
                id: format!("fixture{:04}:{}", n / 4, n % 4),
                group: "all".into(),
                world: [(n % 24) as f64, (n / 24) as f64, 3.],
                uvz: [(n % 24) as f64, (n / 24) as f64, 3.],
            })
            .collect();
        for (form, name) in [
            ("color@1", "Wash"),
            ("color@1", "Chase"),
            ("color@1", "Shimmer"),
            ("color@1", "Drift"),
            ("aim@1", "Circle"),
        ] {
            let inputs = &presets().preset(form, name).unwrap().inputs;
            let program = PreparedGraph::new(
                &library,
                form,
                inputs,
                Frame {
                    cells: &cells,
                    features: None,
                    beat: 0.,
                    clip_start: 0.,
                    clip_duration: 64.,
                    seed: 41,
                },
            )?;
            for i in 0..10 {
                black_box(program.evaluate_batch(&[i as f64 / 60.])?);
            }
            let start = Instant::now();
            let frames = 300;
            for i in 0..frames {
                black_box(program.evaluate_batch(&[1. + i as f64 / 60.])?);
            }
            println!(
                "{count:4} heads {name:8} {:9.2} us/frame ({frames} frames)",
                start.elapsed().as_secs_f64() * 1e6 / frames as f64
            );
        }
    }
    Ok(())
}
