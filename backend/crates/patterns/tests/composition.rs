use luma_patterns::*;

#[test]
fn all_builtins_validate_and_only_forms_play_as_clips() {
    let lib = standard_library();
    for id in lib.definitions.keys() {
        lib.validate(id).unwrap();
    }
    for (id, definition) in &lib.definitions {
        assert!(
            definition
                .inputs
                .values()
                .all(|p| p.value_type != ValueType::Lighting),
            "{id}"
        );
        // Output and the clip forms are the only complete clip graphs.
        assert_eq!(definition.playable(), id == "output" || is_form(id), "{id}");
    }
}

#[test]
fn angled_wings_each_get_their_own_principal_progression() {
    let cells: Vec<_> = (0..2)
        .flat_map(|wing| {
            (0..5).map(move |n| {
                let t = f64::from(n);
                let sign = if wing == 0 { -1.0 } else { 1.0 };
                Cell {
                    id: format!("wing-{wing}/cell-{n}"),
                    group: format!("wing-{wing}"),
                    world: [sign * t, t, 0.0],
                    uvz: [sign * t, t, 0.0],
                }
            })
        })
        .collect();
    let m = MappingSpec {
        span: Default::default(),
        plane: None,
        mirror: None,
        source: MappingSource::MajorAxis {
            toward: [0.0, 1.0, 0.0],
        },
        per_group: true,
        reverse: false,
    }
    .resolve(&cells, 0)
    .unwrap();
    for c in &m.coordinates {
        let n = c.cell.rsplit('-').next().unwrap().parse::<f64>().unwrap();
        assert!((c.position - n / 4.0).abs() < 1e-9);
    }
    let reversed = MappingSpec {
        span: Default::default(),
        plane: None,
        mirror: None,
        source: MappingSource::MajorAxis {
            toward: [0.0, 1.0, 0.0],
        },
        per_group: true,
        reverse: true,
    }
    .resolve(&cells, 0)
    .unwrap();
    for (a, b) in m.coordinates.iter().zip(&reversed.coordinates) {
        assert!((a.position + b.position - 1.0).abs() < 1e-9);
    }
}

#[test]
fn a_perpendicular_major_axis_hint_still_maps_a_horizontal_rig() {
    let cells: Vec<_> = (0..5)
        .map(|i| Cell {
            id: i.to_string(),
            group: "rig".into(),
            world: [i as f64, 0., 2.],
            uvz: [i as f64, 0., 2.],
        })
        .collect();
    let map = MappingSpec {
        span: Default::default(),
        plane: None,
        mirror: None,
        source: MappingSource::MajorAxis {
            toward: [0., 0., 1.],
        },
        per_group: false,
        reverse: false,
    }
    .resolve(&cells, 0)
    .unwrap();
    assert_eq!(
        map.coordinates
            .iter()
            .map(|c| c.position)
            .collect::<Vec<_>>(),
        vec![0., 0.25, 0.5, 0.75, 1.]
    );
}
