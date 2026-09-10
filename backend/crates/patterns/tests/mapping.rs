use luma_patterns::{Cell, MappingSource, MappingSpec, MirrorPlane};

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
fn mirror_retains_the_original_circle_frame_and_wrapping_topology() {
    let cells: Vec<_> = (0..16)
        .map(|index| {
            let angle = index as f64 * std::f64::consts::TAU / 16.;
            cell(&index.to_string(), "circle", [angle.cos(), 0., angle.sin()])
        })
        .collect();
    let mapping = MappingSpec {
        source: MappingSource::Circle { origin: 0.125 },
        mirror: Some(MirrorPlane {
            normal: [1., 0., 0.],
            offset: 0.,
        }),
        reverse: false,
        per_group: false,
    };
    let resolved = mapping.resolve(&cells).expect("mirrored circle");
    for index in 0..16 {
        let reflected = (24 - index) % 16;
        let a = resolved.coordinates[index].position;
        let b = resolved.coordinates[reflected].position;
        let distance = (a - b).abs();
        assert!(distance.min(1. - distance) < 1e-6, "{index}: {a} != {b}");
        assert!(resolved.coordinates[index].closed);
    }
}

#[test]
fn plane_mirror_rejects_nonspatial_order_and_invalid_planes() {
    let mut mapping = MappingSpec {
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
