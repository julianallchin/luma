//! Writes the node reference of the documentation site from
//! `luma_patterns::standard_library()`:
//!
//! `cargo +1.97.1 run --manifest-path backend/crates/patterns/Cargo.toml --example node_reference`
//!
//! Ports, types, rates and defaults come from the library. Categories and the
//! one-line summaries live in `CATEGORIES` below. The run fails when a library
//! definition has no category, or a category names a definition that is gone.
use luma_patterns::{standard_library, Binding, Body, Definition, Input, Library, Rate, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::PathBuf,
};

struct Category {
    slug: &'static str,
    title: &'static str,
    description: &'static str,
    intro: &'static str,
    nodes: &'static [(&'static str, &'static str)],
}

/// Compatibility spellings. The graph editor hides them from its node menu.
const HIDDEN: &[&str] = &["sample_field_envelope", "sample_field_gradient"];

const CATEGORIES: &[Category] = &[
    Category {
        slug: "time-and-events",
        title: "Time and events",
        description: "Clocks, triggers and event queries.",
        intro: "These nodes read musical time and produce events. An event stream is a value of type Events. Nodes such as Pulse, Chase and Dissolve start one response for each event.",
        nodes: &[
            ("clip_time", "Musical time inside the clip: beats since the clip start, progress from 0 to 1, the clip duration in beats and the absolute beat."),
            ("core/track_time", "Track time in seconds, the clip start and duration in seconds, and the track BPM. Needs analyzed track timing."),
            ("rhythm", "A repeating clock. `elapsed` is the beats since the current stroke started. `cycle` is the number of the current stroke, counted from the origin."),
            ("beat_trigger", "Periodic events: one event every Repeat beats, shifted by Phase delay. The origin is beat 0 when Follow track grid is on, and the clip start when it is off."),
            ("core/grid_events", "Events on the analyzed beat grid. Subdivision sets events per beat. Only downbeats uses the downbeats instead of all beats. Beat offset shifts every event by a number of beats."),
            ("drum_trigger", "Events at the analyzed onsets of one drum."),
            ("drum_time", "The beats since the latest onset of one drum, the index of that onset, and whether an onset exists yet."),
            ("core/event_ages", "For the events inside the last Duration beats: elapsed beats, progress from 0 to 1, presence, weight and event index. Each event is one channel."),
            ("core/event_window", "The latest Event count events before the query time. Each event is one channel. Outputs are the event times in seconds, presence, weights and the index of the latest event."),
            ("core/event_spacing", "The smallest gap between two consecutive events that is larger than the ignore threshold, in seconds. Needs recorded events."),
            ("core/thin_events", "Removes each event that comes less than Minimum separation seconds after the previous kept event. Needs recorded events."),
            ("random_subset", "Gives each event a seeded random set of target heads. Proportion sets the fraction of heads. Shuffled cycle walks through a shuffled order to reduce overlap between consecutive events."),
        ],
    },
    Category {
        slug: "audio",
        title: "Audio and music",
        description: "Nodes that read the analyzed audio, stems, drums and harmony of the track.",
        intro: "These nodes read analysis data of the track. A graph that uses them needs that analysis. A missing analysis is an error.",
        nodes: &[
            ("band_energy", "The energy of an audio source between Low frequency and High frequency."),
            ("audio_spectrum", "The magnitude spectrum of an audio source, one channel per frequency bin, and the width of one bin in Hz."),
            ("audio_lowpass", "Adds a lowpass filter to an audio source. Connect the result to Frequency energy or Audio spectrum."),
            ("audio_highpass", "Adds a highpass filter to an audio source. Connect the result to Frequency energy or Audio spectrum."),
            ("harmony", "The current pitch class from the chord analysis, from 0 (C) to 11 (B), and whether a pitch class is present."),
            ("band_mask", "Frequency energy multiplied by Sensitivity, then shaped by the Response curve."),
            ("drum_mask", "A pulse on every onset of one drum."),
        ],
    },
    Category {
        slug: "space",
        title: "Space and geometry",
        description: "Nodes that read head positions and turn them into coordinates.",
        intro: "A head is one independently controllable cell of a fixture. These nodes give values per head. Stage coordinates are U (stage right), V (downstage) and Z (up).",
        nodes: &[
            ("fixture_geometry", "The world position (XYZ) of each head, and its index in the prepared selection."),
            ("stage_coordinates", "The U, V and Z stage coordinates of each head, in meters."),
            ("resolve_mapping", "Resolves a mapping to one coordinate from 0 to 1 per head. The mapping sets the source direction, grouping, reversal and an optional mirror plane."),
            ("mapped_position", "The resolved mapping coordinate of each head."),
            ("coordinate_offset", "The distance of each head's mapped coordinate from Position. When the boundary wraps, the distance wraps into the range −0.5 to 0.5. `wrapped` is 1 for heads that wrap."),
            ("radial_distance", "The distance of each head from the mean U/V center of the selection, in meters."),
            ("core/radial_coordinates", "The angle in turns and the radius of each point around the centroid of the first two position channels."),
            ("core/fit_circle", "Fits a circle to 3D points and gives the angle of each point around it, in turns. If the fit fails, it uses the angle around the centroid."),
            ("core/principal_direction", "The main direction of a 2D point cloud, as a unit vector."),
            ("core/rank_nearby", "Ranks heads by Sort value. A head within Merge distance of the previous head in that order gets the same rank."),
            ("wander_points", "Point count seeded points that drift and oscillate inside a box. The output has three channels (XYZ) per point."),
            ("proximity_weights", "For each position, one weight per point. The weights add up to 1. Nearer points get more weight. Blend distance sets how soft the blend is. At 0 only the nearest point gets weight."),
            ("circle", "The sine of Phase and the sine of Phase + 0.25 turns, as a 2-channel vector."),
            ("normalize_field", "Rescales a value so that the minimum over the selection is 0 and the maximum is 1."),
            ("remap_field", "Rescales a value: (value − Input minimum) ÷ (Input maximum − Input minimum)."),
            ("core/domain_index", "The index of each head in the prepared selection."),
            ("core/align_domain", "Puts Value on the heads of Domain. If Domain has no per-head values, it takes the value of the head with the lowest First fixture order."),
            ("core/field_first", "The value of the head with the lowest Order."),
            ("core/rank", "The rank of each head by value, from 0. Equal values are ordered by head id."),
        ],
    },
    Category {
        slug: "masks",
        title: "Masks and motion",
        description: "Nodes that make per-head brightness masks and movement over time.",
        intro: "A mask is a value from 0 to 1 per head. Multiply a mask by a color to make a lit effect.",
        nodes: &[
            ("pulse", "For each trigger event, samples Shape over Duration beats. Overlapping events combine with the maximum."),
            ("chase", "Each trigger event moves a stroke of Width from Start to End over Travel time, on the mapping. Overlapping strokes combine with the maximum."),
            ("dissolve", "Each trigger event lights a seeded random set of heads. Coverage sets the fraction of heads over the event. Brightness curve sets their level."),
            ("pill", "A stroke of Width centered at Position on the mapping. Shape sets the brightness across the stroke. Activity scales it."),
            ("profile_mask", "Samples Shape across a window of Width centered where the offset is 0."),
            ("noise_mask", "Coherent noise over the mapped U and V positions and clip time, shaped by Response."),
            ("uniform_mask", "The same coverage on every head, clamped to 0–1."),
            ("multiply_mask", "Multiplies two masks and clamps the result to 0–1."),
            ("scale_mask", "Multiplies a mask by Amount and clamps the result to 0–1."),
            ("random_selection", "Selects Proportion of the heads in a random order. A new Index gives a new order. Softness fades the heads near the edge of the selection."),
            ("random_heads_mask", "Lights Lit heads and changes them every Change every beats."),
            ("event_envelope", "Samples Shape at the progress of each event, multiplies by its weight, and takes the maximum over events."),
            ("envelope", "Samples an editable curve at Progress."),
            ("soft_edges", "Makes an envelope that rises and falls linearly at both edges. Edge softness sets the width of the edges."),
            ("motion", "Moves from Start to End along Travel curve over Travel time. Gives the position, the progress and whether the journey is still active."),
        ],
    },
    Category {
        slug: "color",
        title: "Color",
        description: "Nodes that sample gradients, mix palettes and build or rotate colors.",
        intro: "Colors are normalized RGB from 0 to 1. Gradients interpolate in the OKLab color space.",
        nodes: &[
            ("sample_gradient", "The color and opacity of a gradient at Position."),
            ("mix_palette", "Mixes the colors of a palette by channel weights. Gives a color and an opacity."),
            ("palette_fallback", "Gives Palette, or If empty when Palette has no color stops."),
            ("hsv", "Builds an RGB color from hue in turns, saturation and value."),
            ("rotate_hue", "Rotates the hue of a color by a number of turns. The largest and smallest RGB channels keep their values."),
        ],
    },
    Category {
        slug: "effects",
        title: "Effects",
        description: "Complete effect graphs that you can place as clips.",
        intro: "Each of these graphs gives values that Apply accepts. You can place them on the timeline as clips. The [recipes](/docs/node-reference/recipes) page shows how some of them are built.",
        nodes: &[
            ("wash", "One color on every head."),
            ("beat_pulse", "Beat trigger into Pulse, multiplied by Color."),
            ("beat_chase", "Beat trigger into Chase along a mapping, multiplied by Color."),
            ("beat_dissolve", "Beat trigger into Dissolve, multiplied by Color."),
            ("beat_shimmer", "Beat trigger with a short repeat into Dissolve, multiplied by Color."),
            ("band_pulse", "Frequency mask multiplied by Color."),
            ("drum_pulse", "Drum pulse mask multiplied by Color."),
            ("noise_wash", "Noise mask multiplied by Color."),
            ("random_heads", "Random heads mask multiplied by Color."),
            ("rainbow", "The hue turns once every Repeat beats."),
            ("gradient", "Samples the gradient by clip progress."),
            ("spatial_gradient", "Samples the gradient by the mapped position of each head."),
            ("harmony_color", "Maps the current pitch class to a color of the gradient. Dark when no pitch class is present."),
            ("strobe", "Color on every head and a strobe value of Strobe rate."),
        ],
    },
    Category {
        slug: "math",
        title: "Math and signals",
        description: "Arithmetic, channel operations, reductions, noise and random values.",
        intro: "Most of these nodes accept a signal with any unit and any channel layout.",
        nodes: &[
            ("core/add", "A + B."),
            ("core/subtract", "A − B."),
            ("core/multiply", "A × B."),
            ("core/divide", "A ÷ B. Division by zero gives 0."),
            ("core/minimum", "The smaller of A and B."),
            ("core/maximum", "The larger of A and B."),
            ("core/power", "Base raised to Exponent. Both must be dimensionless."),
            ("core/absolute", "The absolute value."),
            ("core/floor", "Rounds down to a whole number."),
            ("core/fraction", "Wraps a value into 0 to 1 (value modulo 1)."),
            ("core/sine", "The sine of a value in turns: sin(2π × value)."),
            ("core/square_root", "The square root. Negative input is an error."),
            ("core/float32", "Rounds a value to 32-bit floating-point precision."),
            ("core/greater", "1 where A − B is greater than Tolerance, 0 elsewhere."),
            ("core/choose", "Yes where Condition is greater than 0, No elsewhere."),
            ("core/choose_number", "Yes when Condition is on, No when it is off."),
            ("core/clamp_coverage", "Clamps a value to 0–1."),
            ("normalize", "Rescales a signal so that its minimum over the clip is 0 and its maximum is 1."),
            ("invert", "Reflects a signal inside its range over the clip: minimum + maximum − value."),
            ("clip_range", "The minimum and maximum of a signal, sampled at evenly spaced times across the clip."),
            ("core/field_minimum", "The minimum over all heads."),
            ("core/field_maximum", "The maximum over all heads."),
            ("core/field_mean", "The mean over all heads."),
            ("core/head_count", "The number of heads."),
            ("core/distinct_count", "The number of different values over all heads."),
            ("core/channel", "One channel of a signal, by index from 0."),
            ("core/join_channels", "Joins the channels of A and B into one signal."),
            ("core/channel_sum", "The sum of all channels."),
            ("core/channel_maximum", "The largest channel value."),
            ("core/channel_argmax", "The index of the largest channel."),
            ("core/channel_index", "The index of each channel, from 0."),
            ("core/channel_count", "The number of channels."),
            ("core/noise", "Smooth 3D noise from 0 to 1. The same coordinates always give the same value."),
            ("core/value_noise_1d", "Seeded 1D value noise with a number of octaves."),
            ("core/value_noise_3d", "Seeded 3D value noise with a number of octaves."),
            ("core/random", "A deterministic random value per head. A different Epoch gives different values."),
            ("core/seed_stream", "Derives a new seed from Seed and Stream."),
        ],
    },
    Category {
        slug: "output",
        title: "Output",
        description: "Apply, the terminal node, and the graphs that feed its capabilities.",
        intro: "Apply is the only node that writes lighting. Every clip graph ends in one Apply.",
        nodes: &[
            ("output", "Writes color, pan, tilt, strobe and movement speed for each head. An unwired input leaves that capability untouched. The brightness of a head is its largest RGB channel. Apply sends it as the dimmer and normalizes the color."),
            ("write_position", "Passes Pan and Tilt through to outputs that connect to Apply."),
            ("write_speed", "Passes a movement speed through to an output that connects to Apply."),
            ("write_strobe", "Gives a strobe value on every head, clamped to 0–1."),
        ],
    },
];

const RECIPES: &[&str] = &[
    "beat_pulse",
    "beat_chase",
    "beat_dissolve",
    "band_pulse",
    "drum_pulse",
    "harmony_color",
    "rainbow",
    "spatial_gradient",
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
        .filter(|id| !HIDDEN.contains(&id.as_str()) && !category_of.contains_key(id.as_str()))
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
    fs::write(dir.join("recipes.mdx"), recipes(&library, &category_of)).unwrap();
    let mut pages = vec!["index".to_string()];
    pages.extend(CATEGORIES.iter().map(|c| c.slug.to_string()));
    pages.push("recipes".into());
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
        Value::Seed(v) => v.to_string(),
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
        Value::AudioSource(_) | Value::Drum(_) | Value::Boundary(_) => match inner.as_str() {
            Some(name) => name.to_string(),
            None => "filtered source".into(),
        },
        Value::Events(_) => match inner["source"].as_str() {
            Some("beats") if inner["times"].as_array().is_some_and(Vec::is_empty) => {
                "no events".into()
            }
            Some("periodic") => {
                format!("every {} beats", number(inner["repeat"].as_f64().unwrap()))
            }
            _ => "events".into(),
        },
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
            .map(|[x, y]| format!("({}, {})", number(*x), number(*y)))
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
        AudioSource => "Audio source".into(),
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
        let mut facts = vec![format!("`{id}`"), kind(definition)];
        if definition.placeable() && !definition.playable() {
            facts.push("Placeable as a clip".into());
        }
        writeln!(out, "{}\n", facts.join(" · ")).unwrap();
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
        "A pattern is a graph. Each node in the graph uses a definition from the standard library. \
A definition has named inputs, named outputs and a body. \
The body is a built-in kernel or another graph. \
Each input of a node is bound to a constant value, to an input of the enclosing graph, or to an output of another node. \
An unbound input uses its default.\n\n\
In the graph editor, the node menu also offers **Input**. \
An Input node adds a named input to the graph.\n\n\
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
        ("Seed", "A 64-bit integer for seeded random nodes."),
        ("Audio source", "The full mix or one stem (bass, drums, vocals, other), with optional lowpass and highpass filters."),
        ("Drum", "One drum class: kick, snare, hi-hat or cymbal."),
        ("Events", "A stream of events: periodic, or recorded times from analysis. Events can carry target heads."),
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
A count of components connects to a named layout with the same number of channels. \
Some nodes, such as Event ages, use channels for a list of events.\n\n",
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
    out.push_str(
        "### [Recipes](/docs/node-reference/recipes)\n\nDiagrams of how the effect graphs are built from other nodes.\n",
    );
    out
}

fn binding_source(binding: &Binding) -> Option<(String, String)> {
    match binding {
        Binding::Input { input } => Some(("inputs".into(), input.clone())),
        Binding::Connection { node, output } => Some((node.clone(), output.clone())),
        Binding::Value { .. } => None,
    }
}

fn recipes(library: &Library, category_of: &BTreeMap<&str, &Category>) -> String {
    let mut out = front_matter(
        "Recipes",
        "How the effect graphs of the standard library are built from other nodes.",
    );
    out.push_str(
        "Each recipe below is a graph in the standard library. \
The diagram shows the nodes of the graph. \
**Inputs** are the inputs of the graph. **Outputs** connect to Apply. \
Constant values show inside the node.\n\n",
    );
    for id in RECIPES {
        let definition = &library.definitions[*id];
        let Body::Graph(graph) = &definition.body else {
            panic!("{id} is not a graph");
        };
        let summary = category_of[id]
            .nodes
            .iter()
            .find(|(node, _)| node == id)
            .unwrap()
            .1;
        writeln!(out, "---\n\n## {}\n", text(&definition.name)).unwrap();
        writeln!(out, "`{id}` · {summary}\n").unwrap();

        // Column of a node: the longest path from the graph inputs.
        fn depth(
            graph: &luma_patterns::Graph,
            id: &str,
            memo: &mut BTreeMap<String, usize>,
        ) -> usize {
            if let Some(d) = memo.get(id) {
                return *d;
            }
            let d = 1 + graph.nodes[id]
                .inputs
                .values()
                .filter_map(|b| match b {
                    Binding::Connection { node, .. } => Some(depth(graph, node, memo)),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            memo.insert(id.into(), d);
            d
        }
        let mut memo = BTreeMap::new();
        let mut columns: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
        for node in graph.nodes.keys() {
            columns
                .entry(depth(graph, node, &mut memo))
                .or_default()
                .push(node);
        }
        let last = columns.keys().max().copied().unwrap_or(0) + 1;

        let mut used_outputs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut edges = Vec::new();
        for (node, spec) in &graph.nodes {
            for (port, binding) in &spec.inputs {
                if let Some((from, handle)) = binding_source(binding) {
                    used_outputs
                        .entry(from.clone())
                        .or_default()
                        .insert(handle.clone());
                    edges.push(serde_json::json!({
                        "from": from, "fromHandle": handle, "to": node, "toHandle": port,
                    }));
                }
            }
        }
        for (port, binding) in &graph.outputs {
            if let Some((from, handle)) = binding_source(binding) {
                used_outputs
                    .entry(from.clone())
                    .or_default()
                    .insert(handle.clone());
                edges.push(serde_json::json!({
                    "from": from, "fromHandle": handle, "to": "outputs", "toHandle": port,
                }));
            }
        }

        let row = |ports: usize| 40 + 18 * ports as i64;
        let mut nodes = vec![serde_json::json!({
            "id": "inputs", "label": "Inputs", "category": "io", "x": 0, "y": 0,
            "outputs": used_outputs.get("inputs").map(|s| s.iter().collect::<Vec<_>>()).unwrap_or_default(),
        })];
        let mut tallest = 0;
        for (column, ids) in &columns {
            let mut y = 0;
            for node in ids {
                let spec = &graph.nodes[*node];
                let inputs: Vec<_> = spec
                    .inputs
                    .iter()
                    .filter(|(_, b)| !matches!(b, Binding::Value { .. }))
                    .map(|(k, _)| k.clone())
                    .collect();
                let params: serde_json::Map<_, _> = spec
                    .inputs
                    .iter()
                    .filter_map(|(k, b)| match b {
                        Binding::Value { value: v } => {
                            Some((k.clone(), serde_json::Value::String(value(v))))
                        }
                        _ => None,
                    })
                    .collect();
                let outputs: Vec<_> = used_outputs
                    .get(*node)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default();
                let category = category_of
                    .get(spec.definition.as_str())
                    .map_or("math", |c| c.slug);
                let ports = inputs.len() + params.len() + outputs.len();
                nodes.push(serde_json::json!({
                    "id": node,
                    "label": library.display_name(&spec.definition),
                    "category": category,
                    "x": *column as i64 * 240,
                    "y": y,
                    "inputs": inputs,
                    "outputs": outputs,
                    "params": params,
                }));
                y += row(ports) + 30;
            }
            tallest = tallest.max(y);
        }
        nodes.push(serde_json::json!({
            "id": "outputs", "label": "Outputs", "category": "io",
            "x": last as i64 * 240, "y": 0,
            "inputs": graph.outputs.keys().collect::<Vec<_>>(),
        }));
        let inputs_height = row(used_outputs.get("inputs").map_or(0, BTreeSet::len)) + 30;
        let height = (tallest.max(inputs_height) + 80).clamp(260, 640);

        writeln!(out, "<NodeGraph\n  height={{{height}}}\n  nodes={{[").unwrap();
        for node in &nodes {
            writeln!(out, "    {node},").unwrap();
        }
        out.push_str("  ]}\n  edges={[\n");
        for edge in &edges {
            writeln!(out, "    {edge},").unwrap();
        }
        out.push_str("  ]}\n/>\n\n");

        out.push_str("### Nodes\n\n");
        let mut ordered: Vec<_> = graph.nodes.keys().collect();
        ordered.sort_by_key(|node| (memo[node.as_str()], node.as_str()));
        for node in ordered {
            let spec = &graph.nodes[node];
            let mut reads = Vec::new();
            for (port, binding) in &spec.inputs {
                reads.push(match binding {
                    Binding::Input { input } => format!("`{port}` from the graph input `{input}`"),
                    Binding::Connection { node, output } => {
                        format!("`{port}` from `{node}.{output}`")
                    }
                    Binding::Value { value: v } => format!("`{port}` = `{}`", value(v)),
                });
            }
            let reads = if reads.is_empty() {
                "It has no bound inputs.".to_string()
            } else {
                format!("It reads {}.", reads.join(", "))
            };
            writeln!(
                out,
                "- **{node}** is {} (`{}`). {reads}",
                text(&library.display_name(&spec.definition)),
                spec.definition
            )
            .unwrap();
        }
        out.push('\n');
        out.push_str("### Inputs\n\n");
        inputs_table(&mut out, &definition.inputs);
    }
    out
}
