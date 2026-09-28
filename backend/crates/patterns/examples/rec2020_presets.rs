//! **One-shot**, deleted with `forms/rec2020_upgrade.rs`. Converts the
//! colors of a `presets.json` from gamma sRGB to linear Rec. 2020 with
//! [`luma_patterns::rec2020_upgrade::convert_clip_inputs`], keeping the
//! file's layout and key order. Only lines whose colors change are written
//! again; a converted number is kept to nine places.
//!
//! `--old-color-forms FILE` converts the inputs of
//! `tests/fixtures/old_color_forms.json` the same way and records their light
//! again, as `color@1` after [`luma_patterns::upgrade`]. The old light was
//! recorded in the old working space, and light is a product of color and
//! brightness that cannot be split after the fact.
//!
//! ```text
//! # write: old file in, converted file out
//! cargo +1.97.1 run --manifest-path backend/crates/patterns/Cargo.toml \
//!   --example rec2020_presets -- OLD.json NEW.json
//! # check: the current file is the old one converted
//! git show <commit before the conversion>:backend/crates/patterns/src/presets.json > /tmp/old.json
//! cargo +1.97.1 run --manifest-path backend/crates/patterns/Cargo.toml \
//!   --example rec2020_presets -- --check /tmp/old.json backend/crates/patterns/src/presets.json
//! ```
use luma_patterns::rec2020_upgrade::convert_clip_inputs;
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Number, Value};
use std::fmt::{self, Write as _};

/// A JSON value that keeps its keys in the order they were written.
enum Ordered {
    Scalar(Value),
    Array(Vec<Ordered>),
    Object(Vec<(String, Ordered)>),
}

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ordered;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON")
            }
            fn visit_bool<E>(self, v: bool) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(json!(v)))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(json!(v)))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(json!(v)))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(json!(v)))
            }
            fn visit_str<E>(self, v: &str) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(json!(v)))
            }
            fn visit_unit<E>(self) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Ordered, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Ordered::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(Ordered::Object(entries))
            }
        }
        deserializer.deserialize_any(V)
    }
}

impl Ordered {
    fn value(&self) -> Value {
        match self {
            Self::Scalar(value) => value.clone(),
            Self::Array(items) => Value::Array(items.iter().map(Self::value).collect()),
            Self::Object(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), value.value()))
                    .collect(),
            ),
        }
    }

    /// Take `new`'s numbers where they differ, to nine places. Whether it
    /// changed anything.
    fn patch(&mut self, new: &Value) -> bool {
        match (self, new) {
            (Self::Array(items), Value::Array(new)) => items
                .iter_mut()
                .zip(new)
                .fold(false, |changed, (item, new)| item.patch(new) | changed),
            (Self::Object(entries), Value::Object(new)) => {
                entries.iter_mut().fold(false, |changed, (key, item)| {
                    item.patch(&new[key.as_str()]) | changed
                })
            }
            (Self::Scalar(Value::Number(old)), Value::Number(new)) => {
                let (a, raw) = (old.as_f64().unwrap(), new.as_f64().unwrap());
                let b = (raw * 1e9).round() / 1e9;
                if a == raw || a == b {
                    return false;
                }
                *old = Number::from_f64(b).unwrap();
                true
            }
            _ => false,
        }
    }

    /// Compact, with the file's spaces.
    fn write(&self, out: &mut String) {
        match self {
            Self::Scalar(value) => out.push_str(&value.to_string()),
            Self::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Self::Object(entries) => {
                out.push('{');
                for (i, (key, item)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write!(out, "{}: ", json!(key)).unwrap();
                    item.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// Equal in shape, numbers within `tolerance`.
fn same(a: &Value, b: &Value, tolerance: f64) -> bool {
    match (a, b) {
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b, tolerance))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same(a, b, tolerance)))
        }
        (Value::Number(a), Value::Number(b)) => {
            (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() <= tolerance
        }
        _ => a == b,
    }
}

fn convert(form: &str, inputs: &Value) -> Value {
    convert_clip_inputs(form, inputs).unwrap_or_else(|error| panic!("{form}: {error}"))
}

fn convert_gradient(gradient: &Value) -> Value {
    let inputs = json!({"g": {"type": "gradient", "value": gradient}});
    convert("", &inputs)["g"]["value"].clone()
}

/// The whole file converted, at full precision.
fn convert_file(old: &Value) -> Value {
    let mut new = old.clone();
    for preset in new["presets"].as_array_mut().unwrap() {
        let form = preset["form"].as_str().unwrap().to_owned();
        preset["inputs"] = convert(&form, &preset["inputs"]);
    }
    for named in new["gradients"].as_array_mut().unwrap() {
        named["gradient"] = convert_gradient(&named["gradient"]);
    }
    new
}

/// The file converted line by line: a preset's input on its own line, and a
/// named gradient on its own line.
fn convert_text(old: &str) -> String {
    let mut form = String::new();
    let mut out = String::new();
    for line in old.lines() {
        let body = line.trim_start();
        let indent = &line[..line.len() - body.len()];
        let (item, comma) = match body.strip_suffix(',') {
            Some(item) => (item, ","),
            None => (body, ""),
        };
        let mut rewritten = None;
        if item.starts_with("{\"name\"") && item.ends_with("\"inputs\": {") {
            let head: Value = serde_json::from_str(&format!("{item}}}}}")).unwrap();
            form = head["form"].as_str().unwrap().to_owned();
        } else if item.starts_with("{\"name\"") && item.contains("\"gradient\": ") {
            let mut named: Ordered = serde_json::from_str(item).unwrap();
            let mut new = named.value();
            new["gradient"] = convert_gradient(&new["gradient"]);
            if named.patch(&new) {
                let mut text = String::new();
                named.write(&mut text);
                rewritten = Some(format!("{indent}{text}{comma}"));
            }
        } else if indent.len() == 6 && item.starts_with('"') {
            let (key, value) = item.split_once(": ").unwrap();
            let key: String = serde_json::from_str(key).unwrap();
            let mut value: Ordered = serde_json::from_str(value).unwrap();
            let new = convert(&form, &json!({ key.clone(): value.value() }));
            if value.patch(&new[&key]) {
                let mut text = String::new();
                value.write(&mut text);
                rewritten = Some(format!("{indent}{}: {text}{comma}", json!(key)));
            }
        }
        out.push_str(rewritten.as_deref().unwrap_or(line));
        out.push('\n');
    }
    out
}

/// Convert the old color forms' recording in place.
fn rerecord(path: &str) {
    use luma_patterns::{standard_library, upgrade, Cell, Frame, PreparedGraph, Value as P};
    use std::collections::BTreeMap;
    let mut recording: Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let cells: Vec<Cell> = serde_json::from_value(recording["cells"].clone()).unwrap();
    let times: Vec<f64> = serde_json::from_value(recording["times"].clone()).unwrap();
    let [start, duration] = ["start", "duration"].map(|key| recording[key].as_f64().unwrap());
    let seed = recording["seed"].as_u64().unwrap();
    let library = standard_library();
    let mut changed = 0;
    for case in recording["cases"].as_array_mut().unwrap() {
        let form = case["form"].as_str().unwrap().to_owned();
        case["inputs"] = convert(&form, &case["inputs"]);
        let inputs: BTreeMap<String, P> = serde_json::from_value(case["inputs"].clone()).unwrap();
        let (form, inputs) = upgrade(&form, &inputs).unwrap();
        let frame = Frame {
            cells: &cells,
            features: None,
            beat: start,
            clip_start: start,
            clip_duration: duration,
            seed,
        };
        let program = PreparedGraph::new(&library, form, &inputs, frame).unwrap();
        let rgb: Vec<Vec<[f64; 3]>> = times
            .iter()
            .map(|t| {
                let result = program.evaluate(start + t).unwrap();
                let P::Lighting(lit) = &result["lighting"] else {
                    panic!("expected lighting")
                };
                cells.iter().map(|cell| lit[&cell.id].rgb()).collect()
            })
            .collect();
        let rgb = json!(rgb);
        if !same(&case["rgb"], &rgb, 1e-7) {
            changed += 1;
        }
        case["rgb"] = rgb;
    }
    std::fs::write(path, serde_json::to_string(&recording).unwrap()).unwrap();
    println!("{changed} cases changed their light");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    match args.as_slice() {
        [check, old, current] if check == "--check" => {
            let old: Value = serde_json::from_str(&read(old)).unwrap();
            let current: Value = serde_json::from_str(&read(current)).unwrap();
            assert!(
                same(&convert_file(&old), &current, 1e-6),
                "the current presets are not the old ones converted"
            );
            println!("the current presets are the old ones converted");
        }
        [flag, path] if flag == "--old-color-forms" => rerecord(path),
        [old, new] => {
            let text = convert_text(&read(old));
            let converted: Value = serde_json::from_str(&text).unwrap();
            let original: Value = serde_json::from_str(&read(old)).unwrap();
            assert!(same(&convert_file(&original), &converted, 1e-6));
            std::fs::write(new, text).unwrap();
        }
        _ => panic!("usage: OLD NEW, or --check OLD CURRENT"),
    }
}
