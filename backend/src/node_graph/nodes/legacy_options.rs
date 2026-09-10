//! Saved node option names, validated by the canonical importer.
pub const MATH_OPS: &[(&str, &str, ())] = &[
    ("add", "Add", ()),
    ("subtract", "Subtract", ()),
    ("multiply", "Multiply", ()),
    ("divide", "Divide", ()),
    ("max", "Max", ()),
    ("min", "Min", ()),
    ("abs_diff", "Absolute Difference", ()),
    ("modulo", "Modulo", ()),
    ("circular_distance", "Circular Distance", ()),
];
pub const ROUND_OPS: &[(&str, &str, ())] = &[
    ("round", "Round", ()),
    ("floor", "Floor", ()),
    ("ceil", "Ceil", ()),
];
pub const ATTRIBUTES: &[(&str, &str, ())] = &[
    ("index", "Index", ()),
    ("normalized_index", "Normalized Index", ()),
    ("count", "Count", ()),
    ("pos_x", "Position X", ()),
    ("pos_y", "Position Y", ()),
    ("pos_z", "Position Z", ()),
    ("rel_x", "Relative X", ()),
    ("rel_y", "Relative Y", ()),
    ("rel_z", "Relative Z", ()),
    ("u", "U (rig axis)", ()),
    ("v", "V (height)", ()),
    ("rel_major_span", "Major Span", ()),
    ("rel_major_count", "Major Count", ()),
    ("angular_position", "Angular Position", ()),
    ("angular_index", "Angular Index", ()),
    ("circle_radius", "Circle Radius", ()),
];
pub const AXES: &[(&str, &str, ())] = &[("x", "X", ()), ("y", "Y", ()), ("z", "Z", ())];
