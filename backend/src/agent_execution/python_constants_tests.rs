//! The Python side keeps its own copy of some Rust constants: the manifest
//! schema it accepts, the PCM header it skips, the names it validates. This
//! reads each copy out of the Python source and checks it against the Rust
//! value, so an edit to one side alone fails here.

use std::collections::BTreeSet;
use std::path::Path;

use luma_patterns::clip_graph::{self, Kind};
use luma_patterns::BlendMode;

use super::artifacts::store::OUTPUTS_DIR;
use super::bindings::providers::features::{DRUM_CLASSES, MERT_FRAME_RATE_HZ};
use super::bindings::{DType, SCHEMA_VERSION};
use super::track_host::LIGHTING_CHANNELS;

/// What a Python assignment must equal.
enum Rust {
    Int(i64),
    Str(&'static str),
    /// The quoted strings, in order.
    List(Vec<String>),
    /// The quoted strings, as a set.
    Set(Vec<String>),
    /// `{"token": "descr"}` pairs that must include each of these.
    Includes(Vec<(String, String)>),
    /// `"name": (index, (labels...))` entries: each index's channel must be
    /// one of the labels it carries.
    Channels(&'static [&'static str]),
    /// Every name is a node id Rust refuses, and every kind but `math` is
    /// among them.
    Reserved,
    /// A name regex whose repetition bound is `{0,n-1}`.
    NameLength(usize),
}

#[test]
fn python_copies_of_rust_constants_agree() {
    let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let table = [
        (
            "luma_exec/bindings.py",
            "SUPPORTED_SCHEMA_VERSION",
            Rust::Int(SCHEMA_VERSION.into()),
        ),
        (
            "luma_exec/bindings.py",
            "PCM_HEADER_BYTES",
            Rust::Int(crate::audio::PCM_HEADER_LEN as i64),
        ),
        (
            "luma_exec/bindings.py",
            "DTYPE_MAP",
            Rust::Includes(
                [DType::F32, DType::F64, DType::F16, DType::I64]
                    .map(|d| (d.as_str().to_string(), d.npy_descr().to_string()))
                    .to_vec(),
            ),
        ),
        (
            "luma_exec/figures.py",
            "OUTPUTS_DIR",
            Rust::Str(OUTPUTS_DIR),
        ),
        (
            "luma_exec/clip.py",
            "VERSION",
            Rust::Int(clip_graph::VERSION.into()),
        ),
        (
            "luma_exec/clip.py",
            "OUTPUTS",
            Rust::Set(
                Kind::ALL
                    .iter()
                    .filter(|kind| kind.is_output())
                    .map(|kind| kind.name().to_string())
                    .collect(),
            ),
        ),
        ("luma_exec/clip.py", "BUILDERS", Rust::Reserved),
        (
            "luma_exec/clip.py",
            "_NAME",
            Rust::NameLength(clip_graph::MAX_ID),
        ),
        (
            "luma_exec/track.py",
            "BLEND_MODES",
            Rust::Set(BlendMode::ALL.map(|mode| mode.name().to_string()).to_vec()),
        ),
        (
            "luma_exec/track.py",
            "_CHANNELS",
            Rust::Channels(&LIGHTING_CHANNELS),
        ),
        (
            "luma_exec/perform.py",
            "BINDING_MODES",
            Rust::Set(
                [
                    crate::inputs::Mode::Fader,
                    crate::inputs::Mode::Momentary,
                    crate::inputs::Mode::Toggle,
                ]
                .map(|mode| {
                    serde_json::to_value(mode)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .to_vec(),
            ),
        ),
        (
            "luma_exec/music.py",
            "DRUMS",
            Rust::List(names(&DRUM_CLASSES)),
        ),
        (
            "mert_worker.py",
            "MERT_FRAMES_PER_SECOND",
            Rust::Int(MERT_FRAME_RATE_HZ as i64),
        ),
        (
            "classifier_worker.py",
            "MERT_FRAMES_PER_SECOND",
            Rust::Int(MERT_FRAME_RATE_HZ as i64),
        ),
    ];

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("python");
    let mut failures = Vec::new();
    for (file, name, rust) in table {
        let source = std::fs::read_to_string(root.join(file)).expect(file);
        let value =
            assignment(&source, name).unwrap_or_else(|| panic!("{file}: no `{name} = ...`"));
        let strings = quoted(value);
        let agrees = match &rust {
            Rust::Int(n) => {
                let literal = value.split('#').next().unwrap_or_default();
                literal.trim().replace('_', "").parse::<i64>().ok() == Some(*n)
            }
            Rust::Str(s) => strings == [*s],
            Rust::List(list) => &strings == list,
            Rust::Set(set) => {
                strings.iter().collect::<BTreeSet<_>>() == set.iter().collect::<BTreeSet<_>>()
            }
            Rust::Includes(pairs) => pairs
                .iter()
                .all(|(token, descr)| strings.windows(2).any(|w| w[0] == *token && w[1] == *descr)),
            Rust::Channels(channels) => {
                let entries: Vec<_> = value.lines().flat_map(channel_entries).collect();
                !entries.is_empty()
                    && entries.iter().all(|(index, labels)| {
                        channels
                            .get(*index)
                            .is_some_and(|channel| labels.iter().any(|l| l == channel))
                    })
            }
            Rust::Reserved => {
                strings
                    .iter()
                    .all(|builder| clip_graph::is_reserved(builder))
                    && clip_graph::is_reserved("math")
                    && Kind::ALL
                        .iter()
                        .filter(|kind| **kind != Kind::Math)
                        .all(|kind| strings.iter().any(|builder| builder == kind.name()))
            }
            Rust::NameLength(max) => value.contains(&format!("{{0,{}}}", max - 1)),
        };
        if !agrees {
            failures.push(format!(
                "{file}: {name} = {} disagrees with Rust",
                value.trim()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The right-hand side of the top-level `name = ...` (or `name: T = ...`) in
/// `source`, through its closing bracket when it spans lines.
fn assignment<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let line = source.lines().find(|line| {
        line.strip_prefix(name)
            .is_some_and(|rest| rest.starts_with(" = ") || rest.starts_with(": "))
    })?;
    let start = line.as_ptr() as usize - source.as_ptr() as usize + line.find(" = ")? + 3;
    let rest = &source[start..];
    let mut depth = 0i32;
    for (at, c) in rest.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '\n' if depth == 0 => return Some(&rest[..at]),
            _ => {}
        }
    }
    Some(rest)
}

/// Every double-quoted string in `text`, comments excluded.
fn quoted(text: &str) -> Vec<String> {
    text.lines()
        .flat_map(|line| {
            let code = line.split(" #").next().unwrap_or_default();
            code.split('"')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `"key": (index, ("label", ...))` entries on one line of a dict literal.
fn channel_entries(line: &str) -> Vec<(usize, Vec<String>)> {
    line.split("\": (")
        .skip(1)
        .filter_map(|entry| {
            let (index, labels) = entry.split_once(", (")?;
            let labels = labels.split("))").next()?;
            Some((index.trim().parse().ok()?, quoted(labels)))
        })
        .collect()
}
