use luma_patterns::{AxisPlane, Cell, MappingSource, MappingSpec, MirrorPlane, Span};
use std::collections::BTreeMap;

fn cell(id: &str, group: &str, uvz: [f64; 3]) -> Cell {
    Cell {
        id: id.into(),
        group: group.into(),
        world: Cell::stage_coordinates(uvz),
        uvz,
    }
}

fn positions(source: MappingSource, cells: &[Cell], reverse: bool, per_group: bool) -> Vec<f64> {
    MappingSpec {
        span: Default::default(),
        plane: None,
        mirror: None,
        source,
        reverse,
        per_group,
    }
    .resolve(cells)
    .expect("valid geometry")
    .coordinates
    .into_iter()
    .map(|coordinate| coordinate.position)
    .collect()
}

fn close(actual: &[f64], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}");
    }
}

#[test]
fn diagonal_uz_projection_is_independent_of_direction_magnitude() {
    let cells = [
        cell("a", "rig", [0., 9., 0.]),
        cell("b", "rig", [1., -3., 0.]),
        cell("c", "rig", [0., 4., 1.]),
        cell("d", "rig", [1., 0., 1.]),
    ];
    for scale in [1e-200, 1., 1e200] {
        let direction = [scale, 0., scale];
        close(
            &positions(MappingSource::Vector { direction }, &cells, false, false),
            &[0., 0.5, 0.5, 1.],
        );
        close(
            &positions(MappingSource::Vector { direction }, &cells, true, false),
            &[1., 0.5, 0.5, 0.],
        );
    }
}

#[test]
fn vector_basis_matches_all_stage_axis_presets() {
    let cells = [
        cell("a", "rig", [0., 1., 2.]),
        cell("b", "rig", [2., 0., 1.]),
        cell("c", "rig", [1., 2., 0.]),
    ];
    for (source, direction) in [
        (MappingSource::U, [1., 0., 0.]),
        (MappingSource::V, [0., 1., 0.]),
        (MappingSource::Z, [0., 0., 1.]),
    ] {
        close(
            &positions(MappingSource::Vector { direction }, &cells, false, false),
            &positions(source, &cells, false, false),
        );
    }
}

#[test]
fn vector_mapping_retains_groups_and_reversed_orientation() {
    let cells = [
        cell("a", "left", [0., 0., 0.]),
        cell("b", "left", [1., 0., 1.]),
        cell("c", "right", [10., 0., 10.]),
        cell("d", "right", [11., 0., 11.]),
    ];
    close(
        &positions(
            MappingSource::Vector {
                direction: [-1., 0., -1.],
            },
            &cells,
            false,
            true,
        ),
        &[1., 0., 1., 0.],
    );
}

#[test]
fn invalid_directions_fail_before_mapping_or_saving() {
    for direction in [[0.; 3], [f64::NAN, 0., 1.], [1., f64::INFINITY, 0.]] {
        let mapping = MappingSpec {
            span: Default::default(),
            plane: None,
            mirror: None,
            source: MappingSource::Vector { direction },
            reverse: false,
            per_group: false,
        };
        assert!(mapping.validate().is_err());
        assert!(mapping.resolve(&[]).is_err());
    }
}

#[test]
fn structured_mapping_survives_value_serialization() {
    let mapping = MappingSpec {
        span: Default::default(),
        plane: None,
        mirror: None,
        source: MappingSource::Vector {
            direction: [1., 0., 2.],
        },
        reverse: true,
        per_group: true,
    };
    let value = luma_patterns::Value::Mapping(mapping);
    let json = serde_json::to_string(&value).expect("serializable value");
    assert_eq!(
        serde_json::from_str::<luma_patterns::Value>(&json).expect("read value"),
        value
    );
}

#[test]
fn mirror_plane_is_independent_of_diagonal_travel_direction() {
    let cells = [
        cell("a", "rig", [-1., 0., 0.]),
        cell("b", "rig", [1., 0., 0.]),
        cell("c", "rig", [-1., 0., 2.]),
        cell("d", "rig", [1., 0., 2.]),
    ];
    let mapping = MappingSpec {
        span: Default::default(),
        plane: None,
        source: MappingSource::Vector {
            direction: [1., 0., 1.],
        },
        mirror: Some(MirrorPlane {
            normal: [1., 0., 0.],
            offset: 0.,
        }),
        reverse: false,
        per_group: false,
    };
    let resolved = mapping.resolve(&cells).expect("diagonal mirrored mapping");
    close(
        &resolved
            .coordinates
            .iter()
            .map(|c| c.position)
            .collect::<Vec<_>>(),
        &[0., 0., 1., 1.],
    );
    let encoded = serde_json::to_string(&mapping).expect("encode mirror");
    assert_eq!(
        serde_json::from_str::<MappingSpec>(&encoded).expect("decode mirror"),
        mapping
    );
}

#[test]
fn mirror_offset_moves_the_plane_without_changing_the_direction() {
    let cells: Vec<_> = (-2..=2)
        .map(|u| cell(&u.to_string(), "rig", [f64::from(u), 0., 0.]))
        .collect();
    let mapping = MappingSpec {
        span: Default::default(),
        plane: None,
        source: MappingSource::U,
        mirror: Some(MirrorPlane {
            normal: [10., 0., 0.],
            offset: 0.5,
        }),
        reverse: false,
        per_group: false,
    };
    let resolved = mapping.resolve(&cells).expect("offset mirror");
    close(
        &resolved
            .coordinates
            .iter()
            .map(|c| c.position)
            .collect::<Vec<_>>(),
        &[1., 0.5, 0., 0., 0.5],
    );
}

#[test]
fn plane_mirror_rejects_nonspatial_order_and_invalid_planes() {
    let mut mapping = MappingSpec {
        span: Default::default(),
        plane: None,
        source: MappingSource::Order,
        mirror: Some(MirrorPlane {
            normal: [1., 0., 0.],
            offset: 0.,
        }),
        reverse: false,
        per_group: false,
    };
    assert!(mapping.validate().is_err());
    mapping.source = MappingSource::U;
    for plane in [
        MirrorPlane {
            normal: [0.; 3],
            offset: 0.,
        },
        MirrorPlane {
            normal: [1., 0., 0.],
            offset: f64::NAN,
        },
    ] {
        mapping.mirror = Some(plane);
        assert!(mapping.validate().is_err());
    }
}

fn by_id(
    source: MappingSource,
    span: Span,
    plane: Option<AxisPlane>,
    cells: &[Cell],
) -> BTreeMap<String, f64> {
    MappingSpec {
        source,
        per_group: false,
        reverse: false,
        mirror: None,
        span,
        plane,
    }
    .resolve(cells)
    .expect("valid geometry")
    .coordinates
    .into_iter()
    .map(|coordinate| (coordinate.cell, coordinate.position))
    .collect()
}

#[test]
fn a_fixture_span_gives_each_fixture_its_own_axis() {
    // Two bars of four heads, far apart, in one group.
    let cells: Vec<Cell> = (0..8)
        .map(|n| {
            let (bar, u) = if n < 4 { ("a", n) } else { ("b", 6 + n) };
            cell(&format!("{bar}:{}", n % 4), "rig", [f64::from(u), 0., 0.])
        })
        .collect();
    let spans = by_id(MappingSource::U, Span::Fixture, None, &cells);
    for head in 0..4 {
        let expected = f64::from(head) / 3.;
        assert!((spans[&format!("a:{head}")] - expected).abs() < 1e-12);
        assert!((spans[&format!("b:{head}")] - expected).abs() < 1e-12);
    }
    // One axis across the selection puts bar b after bar a.
    let whole = by_id(MappingSource::U, Span::Selection, None, &cells);
    assert!(whole["b:0"] > whole["a:3"]);
}

#[test]
fn a_group_span_gives_each_group_its_own_axis() {
    let cells = [
        cell("l1", "left_truss", [0., 0., 0.]),
        cell("l2", "left_truss", [2., 0., 0.]),
        cell("r1", "right_truss", [5., 0., 0.]),
        cell("r2", "right_truss", [9., 0., 0.]),
    ];
    let spans = by_id(MappingSource::U, Span::Group, None, &cells);
    assert_eq!(
        [spans["l1"], spans["l2"], spans["r1"], spans["r2"]],
        [0., 1., 0., 1.]
    );
    // The old per_group flag is the same span.
    let old = positions(MappingSource::U, &cells, false, true);
    close(&old, &[0., 1., 0., 1.]);
    // Both at once is refused.
    let both = MappingSpec {
        source: MappingSource::U,
        per_group: true,
        reverse: false,
        mirror: None,
        span: Span::Fixture,
        plane: None,
    };
    assert!(both.resolve(&cells).is_err());
}

#[test]
fn a_front_facing_ring_with_auto_reads_around_front_back() {
    // Eight heads on a ring standing upright, facing the audience, a little
    // noisy in depth.
    let cells: Vec<Cell> = (0..8)
        .map(|n| {
            let turn = f64::from(n) / 8. * std::f64::consts::TAU;
            let depth = if n % 2 == 0 { 0.01 } else { -0.01 };
            cell(
                &format!("ring:{n}"),
                "rig",
                [5. + 2. * turn.cos(), 3. + depth, 4. + 2. * turn.sin()],
            )
        })
        .collect();
    for source in [MappingSource::Angle, MappingSource::Radial] {
        let auto = by_id(
            source.clone(),
            Span::Selection,
            Some(AxisPlane::Auto),
            &cells,
        );
        let fixed = by_id(source, Span::Selection, Some(AxisPlane::FrontBack), &cells);
        for (id, position) in &auto {
            assert!((position - fixed[id]).abs() < 1e-9, "{id}");
        }
    }
    // Angle 0 is stage right and turns toward up.
    let angle = by_id(
        MappingSource::Angle,
        Span::Selection,
        Some(AxisPlane::Auto),
        &cells,
    );
    assert!(angle["ring:0"].abs() < 1e-9 || (angle["ring:0"] - 1.).abs() < 1e-9);
    assert!((angle["ring:2"] - 0.25).abs() < 1e-9);
    // A custom normal toward upstage is the same plane.
    let custom = by_id(
        MappingSource::Angle,
        Span::Selection,
        Some(AxisPlane::Custom {
            normal: [0., -3., 0.],
        }),
        &cells,
    );
    for (id, position) in &angle {
        assert!((position - custom[id]).abs() < 1e-9, "{id}");
    }
}

#[test]
fn radial_and_angle_center_on_the_centroid() {
    // Heads bunched at one end: the centroid (u 3) is not the middle of the
    // extent (u 4.5).
    let cells = [
        cell("a", "rig", [0., 0., 0.]),
        cell("b", "rig", [1., 0., 0.]),
        cell("c", "rig", [2., 0., 0.]),
        cell("d", "rig", [9., 0., 0.]),
    ];
    let radial = by_id(
        MappingSource::Radial,
        Span::Selection,
        Some(AxisPlane::Auto),
        &cells,
    );
    // Distances 3, 2, 1, 6 from the centroid, scaled over 1..6.
    for (id, expected) in [("a", 0.4), ("b", 0.2), ("c", 0.0), ("d", 1.0)] {
        assert!(
            (radial[id] - expected).abs() < 1e-12,
            "{id}: {}",
            radial[id]
        );
    }
    // Radial and angle need a plane.
    let mut unplaned = MappingSpec {
        span: Span::Selection,
        plane: None,
        mirror: None,
        source: MappingSource::Radial,
        per_group: false,
        reverse: false,
    };
    assert!(unplaned.resolve(&cells).is_err());
    unplaned.source = MappingSource::U;
    unplaned.plane = Some(AxisPlane::Auto);
    assert!(unplaned.resolve(&cells).is_err());
    // Angle around the centroid: a and c sit on opposite sides.
    let angle = by_id(
        MappingSource::Angle,
        Span::Selection,
        Some(AxisPlane::UpDown),
        &cells,
    );
    assert!((angle["a"] - 0.5).abs() < 1e-12);
    assert!(angle["d"].abs() < 1e-12);
}

#[test]
fn a_fixture_span_centers_each_ring_on_itself() {
    // Two rings of four heads, each flat on the floor.
    let mut cells = Vec::new();
    for (ring, center) in [("r1", [0., 0.]), ("r2", [10., 3.])] {
        for n in 0..4 {
            let turn = f64::from(n) / 4. * std::f64::consts::TAU;
            cells.push(cell(
                &format!("{ring}:{n}"),
                "rig",
                [center[0] + turn.cos(), center[1] + turn.sin(), 0.],
            ));
        }
    }
    let angle = by_id(
        MappingSource::Angle,
        Span::Fixture,
        Some(AxisPlane::Auto),
        &cells,
    );
    for n in 0..4 {
        assert!(
            (angle[&format!("r1:{n}")] - angle[&format!("r2:{n}")]).abs() < 1e-9,
            "{n}"
        );
        assert!((angle[&format!("r1:{n}")] - f64::from(n) / 4.).abs() < 1e-9);
    }
}
