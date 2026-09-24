mod support;
use luma_patterns::*;
#[allow(unused_imports)]
use support::EvaluateEffect;

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
fn placement_keeps_auxiliary_outputs_and_rejects_mistyped_capabilities() {
    let mut library = standard_library();
    for id in ["sample_gradient", "sample_field_gradient", "mix_palette"] {
        let definition = &library.definitions[id];
        assert!(definition.placeable());
        let placed = definition.clip_instance(id).unwrap();
        assert!(placed.playable());
        assert!(placed.outputs.contains_key("opacity"));
        library.definitions.insert("placed".into(), placed);
        library.validate("placed").unwrap();
    }
    let mut invalid = library.definitions["sample_gradient"].clone();
    invalid
        .outputs
        .insert("pan".into(), invalid.outputs["color"].clone());
    assert!(
        !invalid.placeable(),
        "a wrong capability type was treated as auxiliary"
    );
    assert!(invalid.clip_instance("sample_gradient").is_err());
    let mut reserved = library.definitions["sample_gradient"].clone();
    reserved
        .outputs
        .insert("lighting".into(), reserved.outputs["opacity"].clone());
    assert!(
        !reserved.placeable(),
        "placement would overwrite an existing output"
    );
    let mut helper = library.definitions["sample_gradient"].clone();
    helper.outputs.remove("color");
    assert!(
        !helper.placeable(),
        "an auxiliary signal has no implicit capability"
    );
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

#[test]
fn internal_layering_preserves_color_when_only_movement_is_written() {
    let mut output = FixtureOutput::from_rgb([0.2, 0.4, 0.8]);
    let original = output.rgb();
    output.composite(
        &FixtureOutput {
            position: Some([0.0, 0.0]),
            ..Default::default()
        },
        BlendMode::Replace,
    );
    assert_eq!(output.rgb(), original);
    assert_eq!(output.position, Some([0.0, 0.0]));
    assert!(output.strobe.is_none());
    output.composite(
        &FixtureOutput {
            dimmer: Some(0.0),
            ..Default::default()
        },
        BlendMode::Replace,
    );
    assert_eq!(output.rgb(), [0.0; 3]);
    assert_eq!(output.position, Some([0.0, 0.0]));
}
