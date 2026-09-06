//! Field-based recipes: masks, geometry and color all compose through the same
//! operations. Nothing here introduces an effect-specific execution kernel.
use crate::{
    recipes::{default, graph, input, node, wire},
    *,
};

fn number(value: f64) -> Binding {
    Value::Number(value).into()
}
fn field(value: f64) -> Node {
    node("core/number_field", &[("value", number(value))])
}
fn binary(op: &str, a: &str, b: &str) -> Node {
    node(op, &[("a", wire(a, "value")), ("b", wire(b, "value"))])
}

pub(crate) fn extend(library: &mut Library) {
    graph(
        library,
        "remap_field",
        "Remap field",
        &[
            ("low", node("core/number_field", &[("value", input("low"))])),
            (
                "high",
                node("core/number_field", &[("value", input("high"))]),
            ),
            ("range", binary("core/subtract", "high", "low")),
            (
                "offset",
                node(
                    "core/subtract",
                    &[("a", input("value")), ("b", wire("low", "value"))],
                ),
            ),
            ("result", binary("core/divide", "offset", "range")),
        ],
        ("value", "result", "value"),
    );
    default(
        library,
        "remap_field",
        "low",
        "Input minimum",
        Value::Number(0.0),
    );
    default(
        library,
        "remap_field",
        "high",
        "Input maximum",
        Value::Number(1.0),
    );

    graph(
        library,
        "normalize_field",
        "Normalize over selection",
        &[
            (
                "low",
                node("core/field_minimum", &[("value", input("value"))]),
            ),
            (
                "high",
                node("core/field_maximum", &[("value", input("value"))]),
            ),
            (
                "result",
                node(
                    "remap_field",
                    &[
                        ("value", input("value")),
                        ("low", wire("low", "value")),
                        ("high", wire("high", "value")),
                    ],
                ),
            ),
        ],
        ("value", "result", "value"),
    );

    let mut nodes = vec![("coordinates", node("stage_coordinates", &[]))];
    for (axis, center, center_field, delta, square) in [
        ("u", "center_u", "mean_u", "u", "u2"),
        ("v", "center_v", "mean_v", "v", "v2"),
    ] {
        nodes.push((
            center,
            node("core/field_mean", &[("value", wire("coordinates", axis))]),
        ));
        nodes.push((
            center_field,
            node("core/number_field", &[("value", wire(center, "value"))]),
        ));
        nodes.push((
            delta,
            node(
                "core/subtract",
                &[
                    ("a", wire("coordinates", axis)),
                    ("b", wire(center_field, "value")),
                ],
            ),
        ));
        nodes.push((square, binary("core/multiply", delta, delta)));
    }
    nodes.extend([
        ("squared", binary("core/add", "u2", "v2")),
        (
            "radius",
            node("core/square_root", &[("value", wire("squared", "value"))]),
        ),
    ]);
    graph(
        library,
        "radial_distance",
        "Distance from selection center (meters)",
        &nodes,
        ("value", "radius", "value"),
    );

    // A profile takes a field of offsets, allowing authors to choose linear,
    // radial, tiled or solved-circle coordinates upstream of the same shape.
    graph(
        library,
        "profile_mask",
        "Shape along a field",
        &[
            (
                "width",
                node("core/number_field", &[("value", input("width"))]),
            ),
            (
                "relative",
                node(
                    "core/divide",
                    &[("a", input("offset")), ("b", wire("width", "value"))],
                ),
            ),
            ("half", field(0.5)),
            ("one", field(1.0)),
            ("zero", field(0.0)),
            ("phase", binary("core/add", "relative", "half")),
            (
                "shape",
                node(
                    "sample_field_envelope",
                    &[("phase", wire("phase", "value")), ("shape", input("shape"))],
                ),
            ),
            ("after", binary("core/greater", "phase", "zero")),
            ("before", binary("core/greater", "one", "phase")),
            ("nonzero", binary("core/greater", "width", "zero")),
            (
                "inside",
                node(
                    "multiply_mask",
                    &[("a", wire("after", "mask")), ("b", wire("before", "mask"))],
                ),
            ),
            (
                "visible",
                node(
                    "multiply_mask",
                    &[
                        ("a", wire("inside", "mask")),
                        ("b", wire("nonzero", "mask")),
                    ],
                ),
            ),
            (
                "result",
                node(
                    "multiply_mask",
                    &[("a", wire("visible", "mask")), ("b", wire("shape", "mask"))],
                ),
            ),
        ],
        ("mask", "result", "mask"),
    );
    default(
        library,
        "profile_mask",
        "width",
        "Width (mapped units)",
        Value::Number(0.25),
    );
    default(
        library,
        "profile_mask",
        "shape",
        "Shape",
        Value::Envelope(Envelope::soft_edges(0.1)),
    );

    graph(
        library,
        "random_heads_mask",
        "Random heads mask",
        &[
            (
                "clock",
                node(
                    "rhythm",
                    &[
                        ("repeat", input("repeat")),
                        ("grid_aligned", input("grid_aligned")),
                        ("delay", input("delay")),
                    ],
                ),
            ),
            (
                "epoch",
                node(
                    "core/choose_number",
                    &[
                        ("condition", input("shuffle")),
                        ("yes", wire("clock", "cycle")),
                        ("no", number(0.0)),
                    ],
                ),
            ),
            (
                "random",
                node("core/random", &[("epoch", wire("epoch", "value"))]),
            ),
            (
                "rank",
                node("core/rank", &[("value", wire("random", "value"))]),
            ),
            (
                "size",
                node("core/head_count", &[("value", wire("rank", "value"))]),
            ),
            (
                "size_field",
                node("core/number_field", &[("value", wire("size", "value"))]),
            ),
            (
                "requested",
                node("core/number_field", &[("value", input("count"))]),
            ),
            (
                "floor",
                node("core/floor", &[("value", wire("requested", "value"))]),
            ),
            ("zero", field(0.0)),
            ("positive", binary("core/maximum", "floor", "zero")),
            ("count", binary("core/minimum", "positive", "size_field")),
            (
                "cycle",
                node(
                    "core/choose_number",
                    &[
                        ("condition", input("shuffle")),
                        ("yes", number(0.0)),
                        ("no", wire("clock", "cycle")),
                    ],
                ),
            ),
            (
                "cycle_field",
                node("core/number_field", &[("value", wire("cycle", "value"))]),
            ),
            ("advance", binary("core/multiply", "count", "cycle_field")),
            ("rotated", binary("core/subtract", "rank", "advance")),
            ("turns", binary("core/divide", "rotated", "size_field")),
            (
                "fraction",
                node("core/fraction", &[("value", wire("turns", "value"))]),
            ),
            ("index", binary("core/multiply", "fraction", "size_field")),
            (
                "selected",
                node(
                    "core/greater",
                    &[
                        ("a", wire("count", "value")),
                        ("b", wire("index", "value")),
                        ("tolerance", number(1e-9)),
                    ],
                ),
            ),
        ],
        ("mask", "selected", "mask"),
    );
    default(
        library,
        "random_heads_mask",
        "count",
        "Lit heads",
        Value::Number(1.0),
    );
    default(
        library,
        "random_heads_mask",
        "repeat",
        "Change every (beats)",
        Value::Beats(1.0),
    );
    default(
        library,
        "random_heads_mask",
        "shuffle",
        "Shuffle each change",
        Value::Boolean(false),
    );
    library.definitions.get_mut("random_heads_mask").unwrap().inputs.get_mut("shuffle").unwrap().description =
        "Off walks through a seeded shuffled order, minimizing repeated heads. On draws a fresh set each change. Counts are floored and bounded by the selection size.".into();
    complete_mask(library, "random_heads", "Random heads", "random_heads_mask");

    graph(
        library,
        "rainbow",
        "Rainbow",
        &[
            (
                "clock",
                node(
                    "rhythm",
                    &[
                        ("repeat", input("repeat")),
                        ("grid_aligned", input("grid_aligned")),
                        ("delay", input("delay")),
                    ],
                ),
            ),
            (
                "elapsed",
                node("core/beats_field", &[("value", wire("clock", "elapsed"))]),
            ),
            (
                "period",
                node("core/beats_field", &[("value", input("repeat"))]),
            ),
            ("phase", binary("core/divide", "elapsed", "period")),
            (
                "color",
                node(
                    "hsv",
                    &[
                        ("hue", wire("phase", "value")),
                        ("saturation", input("saturation")),
                    ],
                ),
            ),
            (
                "output",
                node("core/color_output", &[("color", wire("color", "color"))]),
            ),
        ],
        ("lighting", "output", "lighting"),
    );

    graph(
        library,
        "harmony_color",
        "Harmony color",
        &[
            ("harmony", node("harmony", &[])),
            (
                "position",
                node(
                    "core/divide_number",
                    &[("a", wire("harmony", "pitch_class")), ("b", number(11.0))],
                ),
            ),
            (
                "coverage",
                node(
                    "core/number_coverage",
                    &[("value", wire("position", "value"))],
                ),
            ),
            (
                "color",
                node(
                    "sample_gradient",
                    &[
                        ("position", wire("coverage", "value")),
                        ("gradient", input("gradient")),
                    ],
                ),
            ),
            (
                "present",
                node("uniform_mask", &[("coverage", wire("harmony", "present"))]),
            ),
            (
                "output",
                node(
                    "appearance",
                    &[
                        ("color", wire("color", "color")),
                        ("mask", wire("present", "mask")),
                    ],
                ),
            ),
        ],
        ("lighting", "output", "lighting"),
    );
    let colors = [
        [255, 0, 0],
        [255, 128, 0],
        [255, 204, 0],
        [255, 255, 0],
        [128, 255, 0],
        [0, 255, 0],
        [0, 255, 128],
        [0, 255, 255],
        [0, 128, 255],
        [0, 0, 255],
        [128, 0, 255],
        [255, 0, 128],
    ];
    default(
        library,
        "harmony_color",
        "gradient",
        "Pitch colors (C through B)",
        Value::Gradient(Gradient {
            stops: colors
                .into_iter()
                .enumerate()
                .map(|(i, rgb)| ColorStop {
                    t: i as f64 / 11.0,
                    color: rgb.map(|v| v as f64 / 255.0),
                })
                .collect(),
        }),
    );

    graph(
        library,
        "strobe",
        "Strobe",
        &[
            ("wash", node("wash", &[("color", input("color"))])),
            ("strobe", node("write_strobe", &[("value", input("rate"))])),
            (
                "output",
                node(
                    "add_lighting",
                    &[
                        ("a", wire("wash", "lighting")),
                        ("b", wire("strobe", "lighting")),
                    ],
                ),
            ),
        ],
        ("lighting", "output", "lighting"),
    );
    default(
        library,
        "strobe",
        "rate",
        "Strobe rate (fixture range)",
        Value::Proportion(0.9),
    );
}

fn complete_mask(library: &mut Library, id: &str, name: &str, mask: &str) {
    let bindings: Vec<_> = library.definitions[mask]
        .inputs
        .keys()
        .map(|key| (key.clone(), input(key)))
        .collect();
    let bindings: Vec<_> = bindings
        .iter()
        .map(|(key, value)| (key.as_str(), value.clone()))
        .collect();
    graph(
        library,
        id,
        name,
        &[
            ("mask", node(mask, &bindings)),
            (
                "output",
                node(
                    "appearance",
                    &[("mask", wire("mask", "mask")), ("color", input("color"))],
                ),
            ),
        ],
        ("lighting", "output", "lighting"),
    );
}
