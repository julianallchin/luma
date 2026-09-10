// Standalone reference capture, independent of the replacement evaluator.
// rustc +1.97.1 capture_spectral.rs -o /tmp/luma-capture-spectral
// /tmp/luma-capture-spectral > spectral-v1.json
// Formula copied unchanged from 71320b4a05f97d2edbc2fa10f5ebd8a5dec2c58d:
// src-tauri/src/node_graph/nodes/color.rs, spectral_shift branch.
struct Signal {
    data: Vec<f32>,
}
fn original(rgb: [f32; 3], weights: [f32; 12]) -> [f32; 3] {
    let [r, g, b] = rgb;
    let chroma_sig = Signal {
        data: weights.to_vec(),
    };
    let t = 0;
    let mut out_data = [0.; 3];
    let mut max_p = -1.0;
    let mut dominant_idx = 0;
    for c in 0..12 {
        let p = chroma_sig.data[t * 12 + c];
        if p > max_p {
            max_p = p;
            dominant_idx = c;
        }
    }
    let hue_shift_deg = (dominant_idx as f32 / 12.0) * 360.0;

    let max_c = r.max(g).max(b);
    let min_c = r.min(g).min(b);
    let delta = max_c - min_c;
    let l = (max_c + min_c) / 2.0;
    let mut s = 0.0;
    let mut h = 0.0;
    if delta > 0.00001 {
        s = if l > 0.5 {
            delta / (2.0 - max_c - min_c)
        } else {
            delta / (max_c + min_c)
        };
        if max_c == r {
            h = (g - b) / delta + (if g < b { 6.0 } else { 0.0 });
        } else if max_c == g {
            h = (b - r) / delta + 2.0;
        } else {
            h = (r - g) / delta + 4.0;
        }
        h /= 6.0;
    }
    h = (h + hue_shift_deg / 360.0).fract();
    if h < 0.0 {
        h += 1.0;
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;

    fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * t;
        }
        if t < 1.0 / 2.0 {
            return q;
        }
        if t < 2.0 / 3.0 {
            return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
        }
        p
    }

    out_data[t * 3] = hue_to_rgb(p, q, h + 1.0 / 3.0);
    out_data[t * 3 + 1] = hue_to_rgb(p, q, h);
    out_data[t * 3 + 2] = hue_to_rgb(p, q, h - 1.0 / 3.0);
    out_data
}
fn main() {
    let colors = [
        [1., 0., 0.],
        [0., 1., 0.],
        [0., 0., 1.],
        [1., 1., 1.],
        [0., 0., 0.],
        [0.4, 0.4, 0.4],
        [0.2, 0.6, 0.9],
        [0.01, 0.2, 0.07],
        [0.4, 0.400001, 0.4],
    ];
    let mut weights = Vec::new();
    for pitch in 0..12 {
        let mut value = [0.; 12];
        value[pitch] = 1.;
        weights.push(value);
    }
    weights.push([0.; 12]);
    weights.push([1.; 12]);
    weights.push([-4., -3., -2., -5., -5., -5., -5., -5., -5., -5., -5., -5.]);
    weights.push([0., 0.5, 0.5, 0., 0., 0., 0., 0., 0., 0., 0., 0.]);
    weights.push([-4., -2., -0.5, -3., -4., -4., -4., -4., -4., -4., -4., -4.]);
    println!("[");
    let mut first = true;
    for rgb in colors {
        for &weights in &weights {
            if !first {
                println!(",");
            }
            first = false;
            print!(
                "{{\"rgb\":{:?},\"weights\":{:?},\"expected\":{:?}}}",
                rgb,
                weights,
                original(rgb, weights)
            );
        }
    }
    println!("\n]");
}
