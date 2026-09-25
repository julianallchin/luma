//! Writes the node reference of the documentation site from
//! `luma_patterns::standard_library()`:
//!
//! `cargo +1.97.1 run --manifest-path backend/crates/patterns/Cargo.toml --example node_reference`
//!
//! Ports, types, rates and defaults come from the library. Categories and the
//! one-line summaries live in `CATEGORIES` below. The run fails when a library
//! definition has no category, or a category names a definition that is gone.
use luma_patterns::{standard_library, Body, Definition, Input, Library, Rate, Value};
use std::{collections::BTreeMap, fmt::Write as _, fs, path::PathBuf};

struct Category {
    slug: &'static str,
    title: &'static str,
    description: &'static str,
    intro: &'static str,
    nodes: &'static [(&'static str, &'static str)],
}

const CATEGORIES: &[Category] = &[
    Category {
        slug: "time-and-events",
        title: "Time and events",
        description: "Clocks and the life of each event.",
        intro: "These nodes read musical time. A form counts its events with Event life and reads curves with Curve.",
        nodes: &[
            ("clip_time", "Musical time inside the clip: beats since the clip start, progress from 0 to 1, the clip duration in beats and the absolute beat."),
            ("core/odometer", "Turns counted since the clip start, one turn every Period beats. A period curve changes the period over the clip without a jump."),
            ("core/event_life", "One event every Every beats. Gives the progress of the current event through its Life, from 0 to 1, and the event index."),
            ("core/curve", "Samples a keyframe curve at Progress."),
        ],
    },
    Category {
        slug: "audio",
        title: "Audio",
        description: "Nodes that read the analyzed audio of the track.",
        intro: "These nodes read analysis data of the track. A graph that uses them needs that analysis. A missing analysis is an error.",
        nodes: &[
            ("band_energy", "The energy of the full mix between Low frequency and High frequency."),
        ],
    },
    Category {
        slug: "space",
        title: "Space",
        description: "Nodes that turn head positions into coordinates.",
        intro: "A head is one independently controllable cell of a fixture. These nodes give values per head. Stage coordinates are U (stage right), V (downstage) and Z (up).",
        nodes: &[
            ("resolve_mapping", "Resolves a mapping to one coordinate from 0 to 1 per head. The mapping sets the source direction, grouping, reversal and an optional mirror plane."),
            ("mapped_position", "The resolved mapping coordinate of each head."),
            ("coordinate_offset", "The distance of each head's mapped coordinate from Position. When the boundary wraps, the distance wraps into the range −0.5 to 0.5. `wrapped` is 1 for heads that wrap."),
        ],
    },
    Category {
        slug: "masks",
        title: "Masks",
        description: "Nodes that make per-head brightness masks.",
        intro: "A mask is a value from 0 to 1 per head. Multiply a mask by a color to make a lit effect.",
        nodes: &[
            ("uniform_mask", "The same coverage on every head, clamped to 0–1."),
            ("envelope", "Samples an editable curve at Progress."),
            ("core/random_share", "A seeded random share of the heads for each event index. Coverage sets the share. Grain sets how many heads light together."),
            ("core/path_glides", "Whether a path curve glides: 1 when the curve changes smoothly, 0 when it only jumps."),
        ],
    },
    Category {
        slug: "aim",
        title: "Aim",
        description: "The steps that point moving heads.",
        intro: "An aim is a direction per head in stage U, V, Z. The aim form builds one from these steps.",
        nodes: &[
            ("core/aim_base", "The starting aim: a direction, or the direction from each head to a point."),
            ("core/aim_fan", "Spreads the aims of the heads across the axis by Fan degrees."),
            ("core/aim_motion", "Moves each aim along a shape. Spread sets the phase difference across the axis. Size sets the size in degrees."),
            ("core/aim_offset", "Turns each aim left/right and up/down by a number of degrees."),
        ],
    },
    Category {
        slug: "color",
        title: "Color",
        description: "Gradient sampling.",
        intro: "Colors are normalized RGB from 0 to 1. Gradients interpolate in the OKLab color space.",
        nodes: &[
            ("sample_gradient", "The color and opacity of a gradient at Position."),
        ],
    },
    Category {
        slug: "forms",
        title: "Forms",
        description: "The shipped clip forms. A clip plays one form.",
        intro: "Each form is a graph of the nodes on the other pages. A clip names one form and sets its inputs.",
        nodes: &[
            ("color.constant@1", "All selected heads one color, at a brightness that can follow each hit."),
            ("color.time@1", "A color gradient over time, all heads equal."),
            ("color.space@1", "A gradient laid across the rig. Each head gets a fixed color from its position."),
            ("color.chase@1", "Strokes travel across the heads. Each event starts one stroke."),
            ("color.sparkle@1", "Each event lights a random share of the heads."),
            ("color.noise@1", "Soft brightness that wanders across space and time."),
            ("strobe.constant@1", "Fixture shutter strobe at Rate × Alpha."),
            ("aim@1", "Points moving heads: a base aim, a fan across the axis and a motion shape."),
        ],
    },
    Category {
        slug: "math",
        title: "Math and signals",
        description: "Arithmetic, channel operations and noise.",
        intro: "Most of these nodes accept a signal with any unit and any channel layout.",
        nodes: &[
            ("core/add", "A + B."),
            ("core/subtract", "A − B."),
            ("core/multiply", "A × B."),
            ("core/divide", "A ÷ B. Division by zero gives 0."),
            ("core/minimum", "The smaller of A and B."),
            ("core/maximum", "The larger of A and B."),
            ("core/fraction", "Wraps a value into 0 to 1 (value modulo 1)."),
            ("core/greater", "1 where A − B is greater than Tolerance, 0 elsewhere."),
            ("core/choose_number", "Yes when Condition is on, No when it is off."),
            ("core/clamp_coverage", "Clamps a value to 0–1."),
            ("normalize", "Rescales a signal so that its minimum over the clip is 0 and its maximum is 1."),
            ("clip_range", "The minimum and maximum of a signal, sampled at evenly spaced times across the clip."),
            ("core/join_channels", "Joins the channels of A and B into one signal."),
            ("core/channel_maximum", "The largest channel value."),
            ("core/noise", "Smooth 3D noise from 0 to 1. The same coordinates always give the same value."),
        ],
    },
    Category {
        slug: "output",
        title: "Output",
        description: "Apply, the terminal node, and the graphs that feed its capabilities.",
        intro: "Apply is the only node that writes lighting. Every clip graph ends in one Apply.",
        nodes: &[
            ("output", "Writes color, pan, tilt, strobe and movement speed for each head. An unwired input leaves that capability untouched. The brightness of a head is its largest RGB channel. Apply sends it as the dimmer and normalizes the color."),
            ("write_strobe", "Gives a strobe value on every head, clamped to 0–1."),
        ],
    },
];

const GENERATED: &str = "{/* Generated by backend/crates/patterns/examples/node_reference.rs from luma_patterns::standard_library(). Do not edit this file. Run the example again. */}";

fn main() {
    let library = standard_library();
    let mut category_of = BTreeMap::new();
    for category in CATEGORIES {
        for (id, _) in category.nodes {
            assert!(
                library.definitions.contains_key(*id),
                "{id} is not in the standard library"
            );
            assert!(
                category_of.insert(*id, category).is_none(),
                "{id} is in two categories"
            );
        }
    }
    let missing: Vec<_> = library
        .definitions
        .keys()
        .filter(|id| !category_of.contains_key(id.as_str()))
        .collect();
    assert!(missing.is_empty(), "no category for {missing:?}");

    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../www/content/docs/node-reference");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("index.mdx"), index(&library)).unwrap();
    for category in CATEGORIES {
        fs::write(
            dir.join(format!("{}.mdx", category.slug)),
            category_page(&library, category),
        )
        .unwrap();
    }
    let mut pages = vec!["index".to_string()];
    pages.extend(CATEGORIES.iter().map(|c| c.slug.to_string()));
    fs::write(
        dir.join("meta.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "title": "Node Reference",
            "pages": pages,
        }))
        .unwrap()
            + "\n",
    )
    .unwrap();
}

fn front_matter(title: &str, description: &str) -> String {
    format!("---\ntitle: {title}\ndescription: {description}\n---\n\n{GENERATED}\n\n")
}

/// Text inside a table cell or a paragraph.
fn text(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('<', "&lt;")
}

fn number(value: f64) -> String {
    let rounded = format!("{value:.3}");
    let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
    if trimmed == "-0" { "0" } else { trimmed }.to_string()
}

fn numbers(values: impl IntoIterator<Item = f64>) -> String {
    values
        .into_iter()
        .map(number)
        .collect::<Vec<_>>()
        .join(", ")
}

fn value(value: &Value) -> String {
    let json = serde_json::to_value(value).unwrap();
    let inner = &json["value"];
    match value {
        Value::Number(v) | Value::Position(v) | Value::Proportion(v) => number(*v),
        Value::Beats(v) => format!("{} beats", number(*v)),
        Value::Degrees(v) => format!("{}°", number(*v)),
        Value::Seconds(v) => format!("{} s", number(*v)),
        Value::Boolean(v) => if *v { "on" } else { "off" }.into(),
        Value::Color(rgb) => format!("RGB ({})", numbers(*rgb)),
        Value::Signal(_) => format!(
            "({})",
            numbers(
                inner["values"]["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_f64().unwrap())
            )
        ),
        Value::Boundary(_) => inner.as_str().unwrap().to_string(),
        Value::Gradient(gradient) => match gradient.stops.len() {
            0 => "no color stops".into(),
            1..=3 => gradient
                .stops
                .iter()
                .map(|stop| format!("RGB ({}) at {}", numbers(stop.color), number(stop.t)))
                .collect::<Vec<_>>()
                .join(", "),
            count => format!("{count} color stops"),
        },
        Value::Envelope(envelope) => envelope
            .points
            .iter()
            .map(|point| format!("({}, {})", number(point.x), number(point.value)))
            .collect::<Vec<_>>()
            .join(" "),
        Value::Mapping(spec) => {
            let mut words = vec![format!("source {}", spec.source.key())];
            if spec.per_group {
                words.push("per group".into());
            }
            if spec.reverse {
                words.push("reversed".into());
            }
            words.join(", ")
        }
        _ => "—".into(),
    }
}

fn type_name(kind: luma_patterns::ValueType) -> String {
    use luma_patterns::ValueType::*;
    match kind {
        ColorField => "Color field".into(),
        other => other.to_string(),
    }
}

fn rate(rate: Rate) -> &'static str {
    match rate {
        Rate::Fixed => "fixed",
        Rate::Frame => "per frame",
    }
}

fn inputs_table(out: &mut String, inputs: &BTreeMap<String, Input>) {
    if inputs.is_empty() {
        out.push_str("No inputs.\n\n");
        return;
    }
    out.push_str("| Input | Name | Type | Rate | Default | Description |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for (key, input) in inputs {
        let description = if input.description.is_empty() || input.description == input.name {
            String::new()
        } else {
            text(&input.description)
        };
        let default = match &input.default {
            Some(v) => format!("`{}`", value(v)),
            None => "required".into(),
        };
        writeln!(
            out,
            "| `{key}` | {} | {} | {} | {default} | {description} |",
            text(&input.name),
            text(&type_name(input.value_type)),
            rate(input.rate),
        )
        .unwrap();
    }
    out.push('\n');
}

fn outputs_table(out: &mut String, definition: &Definition) {
    out.push_str("| Output | Type | Rate |\n|---|---|---|\n");
    for (key, output) in &definition.outputs {
        writeln!(
            out,
            "| `{key}` | {} | {} |",
            text(&type_name(output.value_type)),
            rate(output.rate)
        )
        .unwrap();
    }
    out.push('\n');
}

fn kind(definition: &Definition) -> String {
    match &definition.body {
        Body::Primitive(_) => "Built-in kernel".into(),
        Body::Graph(graph) => match graph.nodes.len() {
            0 => "Graph with no nodes".into(),
            1 => "Graph of 1 node".into(),
            n => format!("Graph of {n} nodes"),
        },
    }
}

fn category_page(library: &Library, category: &Category) -> String {
    let mut out = front_matter(category.title, category.description);
    writeln!(out, "{}\n", category.intro).unwrap();
    for (id, summary) in category.nodes {
        let definition = &library.definitions[*id];
        writeln!(out, "## {}\n", text(&library.display_name(id))).unwrap();
        writeln!(out, "`{id}` · {}\n", kind(definition)).unwrap();
        writeln!(out, "{summary}\n").unwrap();
        inputs_table(&mut out, &definition.inputs);
        outputs_table(&mut out, definition);
    }
    out
}

fn index(library: &Library) -> String {
    let mut out = front_matter(
        "Node Reference",
        "Every node in the Luma pattern graph standard library, with its ports, types and defaults.",
    );
    out.push_str(
        "A clip form is a graph. Each node in the graph uses a definition from the standard library. \
A definition has named inputs, named outputs and a body. \
The body is a built-in kernel or another graph. \
Each input of a node is bound to a constant value, to an input of the enclosing graph, or to an output of another node. \
An unbound input uses its default.\n\n\
A clip graph ends in an [Apply](/docs/node-reference/output) node. \
Apply is the only node that writes lighting.\n\n",
    );
    out.push_str("## Values\n\n");
    out.push_str(
        "Numerical wires carry a **signal**. A signal is a three-axis tensor: heads × time samples × channels. \
A signal also has a unit and a channel layout. \
A signal without a head list is the same for every head.\n\n",
    );
    out.push_str("| Type | Meaning |\n|---|---|\n");
    for (name, meaning) in [
        ("Signal", "A tensor of heads × time × channels, with a unit and a channel layout. Units or channels that a port leaves open are inferred from the connected wire."),
        ("Number", "A signal with the unit number and one channel."),
        ("Beats", "A signal with the unit beats and one channel."),
        ("Proportion", "A signal with the unit 0–1 and one channel."),
        ("Position", "A signal with the unit position and one channel. A position is a coordinate on a mapping."),
        ("Color", "A signal with the unit 0–1 and RGB channels."),
        ("Color field", "The same as Color: RGB from 0 to 1, per head."),
        ("Field", "The same as Number. The name marks a value per head."),
        ("Mask", "A value from 0 to 1 per head."),
        ("Boolean", "On or off."),
        ("Gradient", "Color stops from 0 to 1. Each stop has a color and an alpha."),
        ("Envelope", "An editable curve through points from 0 to 1."),
        ("Mapping", "How to give each head a coordinate: a source direction, per group or not, reversed or not, and an optional mirror plane."),
        ("Coordinates", "A resolved mapping: one position from 0 to 1 per head, and whether the mapping is closed."),
        ("Boundary", "What happens at the ends of a mapping: natural (follow the mapping), clip or wrap."),
        ("Lighting", "Per-head output: color, dimmer, pan/tilt, strobe and speed. Only Apply gives Lighting."),
    ] {
        writeln!(out, "| {name} | {} |", text(meaning)).unwrap();
    }
    out.push('\n');
    out.push_str("### Units\n\n");
    out.push_str(
        "Signal units are number, beats, 0–1 (proportion), position, degrees and seconds. \
Number, 0–1 and position are dimensionless, and a port that expects one of them accepts the others. \
Beats, degrees and seconds only connect to the same unit.\n\n",
    );
    out.push_str("### Channels\n\n");
    out.push_str(
        "A channel layout is one channel, RGB, pan/tilt, or a count of unnamed components. \
A count of components connects to a named layout with the same number of channels.\n\n",
    );
    out.push_str("### Rates\n\n");
    out.push_str(
        "A **fixed** port has one value for the whole clip. A **per frame** port can change every frame. \
A per-frame wire cannot connect to a fixed input.\n\n",
    );
    out.push_str("## Categories\n\n");
    for category in CATEGORIES {
        writeln!(
            out,
            "### [{}](/docs/node-reference/{})\n\n{}\n",
            category.title, category.slug, category.description
        )
        .unwrap();
        for (id, _) in category.nodes {
            writeln!(out, "- {} (`{id}`)", text(&library.display_name(id))).unwrap();
        }
        out.push('\n');
    }
    out
}
