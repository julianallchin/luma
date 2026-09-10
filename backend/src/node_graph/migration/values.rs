//! Decoding for original stored numerical graph values.
use crate::models::node_graph::Stops;
use serde_json::Value;

pub(crate) fn parse_stops(v: &Value) -> Stops {
    let mut stops: Vec<(f32, [f32; 4])> = Vec::new();
    if let Some(arr) = v.get("stops").and_then(|s| s.as_array()) {
        for s in arr {
            let t = s.get("t").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
            let mut rgba = s
                .get("color")
                .map(parse_color)
                .unwrap_or([0.0, 0.0, 0.0, 1.0]);
            if let Some(alpha) = s.get("alpha").and_then(Value::as_f64) {
                rgba[3] = alpha as f32;
            }
            stops.push((t, rgba));
        }
    } else if let Some(colors) = v.get("colors").and_then(|c| c.as_array()) {
        let n = colors.len();
        for (i, c) in colors.iter().enumerate() {
            let t = if n <= 1 {
                0.0
            } else {
                i as f32 / (n - 1) as f32
            };
            stops.push((t, parse_color(c)));
        }
    }
    stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Stops { stops }
}

/// Parse a color value — a hex string (`"#rrggbb[aa]"`) or an `{r,g,b,a}` object
/// (r/g/b in 0–255, a in 0–1) — into normalized RGBA.
pub(crate) fn parse_color(v: &Value) -> [f32; 4] {
    if let Some(hex) = v.as_str() {
        return parse_hex(hex);
    }
    let f = |k: &str, d: f64| v.get(k).and_then(|x| x.as_f64()).unwrap_or(d) as f32;
    [
        f("r", 0.0) / 255.0,
        f("g", 0.0) / 255.0,
        f("b", 0.0) / 255.0,
        f("a", 1.0),
    ]
}

pub(crate) fn parse_hex(h: &str) -> [f32; 4] {
    let h = h.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f32 / 255.0;
    if h.is_ascii() && h.len() >= 6 {
        [
            byte(0),
            byte(2),
            byte(4),
            if h.len() >= 8 { byte(6) } else { 1.0 },
        ]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

pub(crate) fn parse_node_color(v: &serde_json::Value) -> [f32; 4] {
    match v {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('#') {
                return parse_hex(trimmed);
            }
            // Legacy stores the object as a JSON-encoded text param.
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                return parse_node_color(&parsed);
            }
            [1.0, 0.0, 0.0, 1.0]
        }
        serde_json::Value::Object(_) => {
            let chan = |k: &str, d: f64| v.get(k).and_then(|x| x.as_f64()).unwrap_or(d);
            [
                (chan("r", 255.0) / 255.0) as f32,
                (chan("g", 0.0) / 255.0) as f32,
                (chan("b", 0.0) / 255.0) as f32,
                // alpha is 0..1 in the legacy node (see color.rs:466).
                chan("a", 1.0) as f32,
            ]
        }
        _ => [1.0, 0.0, 0.0, 1.0],
    }
}
