//! Fixture-shadow and sun-cascade caching, culling and invalidation.

use glam::{Mat4, Vec3};

use crate::assets::Material;
use crate::frame::{Camera, DirectionalLight, Draw, MaterialTextures, MeshData};
use crate::light_index::cone_reaches_sphere;

use super::fixture_surface_frame;
use crate::gpu::Renderer;

#[test]
fn fixture_geometry_casts_a_shadow_through_the_volumetric_integral() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let mut frame = fixture_surface_frame(1);
    frame.fixture_cones[0].position = Vec3::new(0.0, 0.0, 3.0);
    frame.draws.push(Draw {
        mesh: 0,
        model: Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0))
            * Mat4::from_scale(Vec3::new(0.09, 0.09, 0.09)),
        material: Material {
            base_color: Vec3::splat(0.08),
            roughness: 0.8,
            ..Material::default()
        },
        textures: MaterialTextures::default(),
        editor_object: None,
    });
    frame.camera = Camera {
        eye: Vec3::new(3.8, -6.0, 3.2),
        target: Vec3::new(0.0, 0.0, 1.35),
        fov_y_deg: 45.0,
    };
    frame.ambient = Vec3::ZERO;
    frame.haze_steps = 8;
    frame.haze_resolution = 1.0;
    let occluder_ndc = crate::shadow::fixture_shadow_matrix(&frame.fixture_cones[0])
        .project_point3(Vec3::new(0.0, 0.0, 2.0));
    assert!(
        occluder_ndc.x.abs() < 1.0
            && occluder_ndc.y.abs() < 1.0
            && (0.0..=1.0).contains(&occluder_ndc.z),
        "the shadow contract's occluder must remain inside the fixture projection: {occluder_ndc:?}"
    );

    frame.haze_density = 0.0;
    frame.fixture_surface_lighting = true;
    frame.fixture_shadows = false;
    let surface_open = renderer.render(&frame, 320, 240, 1)?;
    frame.fixture_shadows = true;
    let surface_shadowed = renderer.render(&frame, 320, 240, 1)?;
    let surface_changed = surface_open
        .chunks_exact(4)
        .zip(surface_shadowed.chunks_exact(4))
        .filter(|(left, right)| left[..3] != right[..3])
        .count();
    assert!(
        surface_changed > 300,
        "the fixture shadow map did not alter a lit surface: {surface_changed} pixels"
    );

    frame.haze_density = 0.8;
    frame.fixture_surface_lighting = false;
    frame.fixture_shadows = false;
    let open = renderer.render(&frame, 320, 240, 2)?;
    frame.fixture_shadows = true;
    let shadowed = renderer.render(&frame, 320, 240, 2)?;

    let changed = open
        .chunks_exact(4)
        .zip(shadowed.chunks_exact(4))
        .filter(|(left, right)| left[..3] != right[..3])
        .count();
    let energy = |pixels: &[u8]| {
        pixels
            .chunks_exact(4)
            .map(|pixel| u64::from(pixel[0]) + u64::from(pixel[1]) + u64::from(pixel[2]))
            .sum::<u64>()
    };
    assert!(
        changed > 300,
        "the fixture shadow map did not alter a visible volume: {changed} pixels"
    );
    assert!(
        energy(&shadowed) < energy(&open),
        "shadow visibility added energy instead of removing in-scatter"
    );
    Ok(())
}

#[test]
fn cached_shadows_survive_blackout_reordering_and_colour_changes() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let mut frame = fixture_surface_frame(300);
    frame.geometry_shadows = true;
    frame.haze_density = 0.0;
    renderer.render(&frame, 80, 60, 1)?;
    assert_eq!(renderer.shadowed_fixture_count(), 300);
    assert_eq!(renderer.shadow_stats().redrawn_maps, 300);
    assert_eq!(renderer.shadow_stats().hierarchy_layers, 300);
    let mut lights = std::mem::take(&mut frame.fixture_cones);
    renderer.render(&frame, 80, 60, 1)?;
    assert_eq!(renderer.shadow_stats().hierarchy_layers, 0);
    lights.reverse();
    for light in &mut lights {
        light.color = Vec3::X;
        light.intensity *= 0.5;
    }
    frame.fixture_cones = lights;
    renderer.render(&frame, 80, 60, 1)?;
    assert_eq!(
        renderer.shadow_stats().redrawn_maps,
        0,
        "blackout and colour are not geometry changes"
    );
    assert_eq!(renderer.shadow_stats().hierarchy_layers, 0);
    frame.fixture_cones[0].direction = Vec3::new(0.1, 0.0, -1.0).normalize();
    renderer.render(&frame, 80, 60, 1)?;
    assert_eq!(
        renderer.shadow_stats().redrawn_maps,
        1,
        "only the moved light needs a new map"
    );
    assert_eq!(renderer.shadow_stats().hierarchy_layers, 1);
    frame.draws[0].model = Mat4::from_translation(Vec3::new(0.0, 0.0, -0.1));
    renderer.render(&frame, 80, 60, 1)?;
    assert_eq!(
        renderer.shadow_stats().redrawn_maps,
        300,
        "stage edits invalidate old depth"
    );
    assert_eq!(renderer.shadow_stats().hierarchy_layers, 300);
    Ok(())
}

#[test]
fn shadow_capacity_hint_avoids_atlas_growth_during_cue_ramp() -> anyhow::Result<()> {
    let mut renderer = Renderer::new()?;
    let mut full = fixture_surface_frame(340);
    full.geometry_shadows = true;
    full.haze_density = 0.0;
    let lights = std::mem::take(&mut full.fixture_cones);
    // Public callers can construct a frame without the scene builder's
    // clamp. The allocator owns its shader/resource bound as well.
    full.fixture_shadow_capacity_hint = usize::MAX;

    renderer.render(&full, 80, 60, 1)?;
    assert_eq!(renderer.fixture_shadow_cache.len(), 512);
    assert_eq!(renderer.shadow_stats().redrawn_maps, 0);
    full.fixture_shadow_capacity_hint = 470;
    let cache_address = renderer.fixture_shadow_cache.as_ptr();
    let mut previous = Vec::new();
    let mut hinted = Vec::new();
    for (count, expected_redraws) in [(34, 34), (210, 176), (340, 130)] {
        full.fixture_cones = lights[..count].to_vec();
        hinted = renderer.render(&full, 80, 60, 1)?;
        assert_eq!(renderer.fixture_shadow_cache.len(), 512);
        assert_eq!(renderer.fixture_shadow_cache.as_ptr(), cache_address);
        assert_eq!(renderer.shadow_stats().redrawn_maps, expected_redraws);
        assert_eq!(&renderer.fixture_shadow_cache[..previous.len()], &previous);
        previous = renderer.fixture_shadow_cache[..count].to_vec();
    }

    let mut fresh = fixture_surface_frame(340);
    fresh.geometry_shadows = true;
    fresh.haze_density = 0.0;
    fresh.fixture_shadow_capacity_hint = 0;
    let reference = Renderer::new()?.render(&fresh, 80, 60, 1)?;
    let max_delta = hinted
        .iter()
        .zip(&reference)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert!(
        max_delta <= 1,
        "capacity hint changed the final image (max {max_delta})"
    );
    Ok(())
}

#[test]
fn cached_shadows_clear_when_the_last_caster_is_removed() -> anyhow::Result<()> {
    let mut frame = fixture_surface_frame(1);
    frame.geometry_shadows = true;
    frame.fixture_surface_lighting = false;
    frame.haze_density = 0.6;
    frame.haze_steps = 8;
    frame.draws[0].model = Mat4::from_translation(Vec3::new(0.0, 0.0, 1.5));
    let mut renderer = Renderer::new()?;
    renderer.render(&frame, 160, 120, 1)?;
    frame.draws.clear();
    let cleared = renderer.render(&frame, 160, 120, 1)?;
    assert_eq!(renderer.shadow_stats().redrawn_maps, 1);
    assert_eq!(renderer.shadow_stats().hierarchy_layers, 1);
    let fresh = Renderer::new()?.render(&frame, 160, 120, 1)?;
    assert!(fresh.chunks_exact(4).any(|pixel| pixel[..3] != [0; 3]));
    assert_eq!(cleared, fresh, "removed geometry left a stale haze shadow");
    assert_eq!(renderer.render(&frame, 160, 120, 1)?, fresh);
    assert_eq!(renderer.shadow_stats().hierarchy_layers, 0);
    Ok(())
}

/// A moving head is opaque geometry in the sun cascades. Its body must
/// invalidate them when it moves, or the sun keeps its old shadow.
#[test]
fn sun_cascades_redraw_when_a_fixture_body_moves() -> anyhow::Result<()> {
    let mut frame = fixture_surface_frame(0);
    frame.fixture_shadows = false;
    frame.fixture_surface_lighting = false;
    frame.directional = Some(DirectionalLight {
        direction: Vec3::new(0.3, -0.2, 1.0).normalize(),
        radiance: Vec3::splat(3.0),
        shadow_eye: Vec3::ZERO,
        shadows: true,
        shadow_softness: 1.0,
    });
    let body = frame.meshes.len();
    frame.meshes.push(MeshData {
        key: "::moving-head-body".into(),
        ..frame.meshes[0].clone()
    });
    frame.draws.push(Draw {
        mesh: body,
        model: Mat4::from_translation(Vec3::new(-2.0, 0.0, 1.0))
            * Mat4::from_scale(Vec3::new(0.2, 0.2, 1.0)),
        material: frame.draws[0].material,
        textures: MaterialTextures::default(),
        editor_object: Some(crate::frame::EditorObject::Fixture("head".into())),
    });
    let opaque = frame.draws.len();
    let before_hash = crate::gpu::camera::opaque_depth_hash(&frame, opaque);
    let fixture_casters = crate::shadow::fixture_shadow_caster_hash(&frame, opaque);

    let mut renderer = Renderer::new()?;
    let before = renderer.render(&frame, 160, 120, 1)?;
    frame.draws[1].model = Mat4::from_translation(Vec3::new(2.0, 0.0, 1.0))
        * Mat4::from_scale(Vec3::new(0.2, 0.2, 1.0));
    assert_eq!(
        crate::shadow::fixture_shadow_caster_hash(&frame, opaque),
        fixture_casters,
        "fixture maps deliberately ignore fixture bodies"
    );
    assert_ne!(
        crate::gpu::camera::opaque_depth_hash(&frame, opaque),
        before_hash
    );
    let moved = renderer.render(&frame, 160, 120, 1)?;
    let fresh = Renderer::new()?.render(&frame, 160, 120, 1)?;
    assert_ne!(before, fresh, "the moved body must change the picture");
    assert_eq!(moved, fresh, "a moved fixture body left a stale sun shadow");
    Ok(())
}

#[test]
fn geometry_visibility_shadows_a_fixture_beyond_the_atlas_budget() -> anyhow::Result<()> {
    const DECOYS: usize = 300;
    let mut renderer = Renderer::new()?;
    renderer.set_geometry_shadows(false);
    let mut frame = fixture_surface_frame(1);
    let mut light = frame.fixture_cones[0];
    light.position = Vec3::new(0.0, 0.0, 3.0);
    light.intensity = 1.0;
    let mut decoy = light;
    decoy.direction = Vec3::Z;
    decoy.intensity = 10.0;
    frame.fixture_cones = vec![decoy; DECOYS];
    frame.fixture_cones.push(light);
    frame.fixture_shadows = true;
    frame.ambient = Vec3::ZERO;
    frame.draws.push(Draw {
        mesh: 0,
        model: Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0))
            * Mat4::from_scale(Vec3::splat(0.09)),
        material: Material::default(),
        textures: MaterialTextures::default(),
        editor_object: None,
    });
    for haze in [false, true] {
        frame.fixture_surface_lighting = !haze;
        frame.haze_density = if haze { 0.8 } else { 0.0 };
        frame.geometry_shadows = false;
        let open = renderer.render(&frame, 160, 120, 2)?;
        assert!(!renderer.fixture_shadow_slots.contains(&Some(DECOYS)));
        frame.geometry_shadows = true;
        let blocked = renderer.render(&frame, 160, 120, 2)?;
        let energy = |p: &[u8]| {
            p.chunks_exact(4)
                .map(|p| p[..3].iter().map(|v| *v as u64).sum::<u64>())
                .sum::<u64>()
        };
        assert!(
            energy(&blocked) < energy(&open),
            "second shadow bank must remove occluded energy, haze={haze}"
        );
        // Moving the stage caster invalidates the cached acceleration structure.
        frame.draws[1].model = Mat4::from_translation(Vec3::new(20.0, 0.0, 2.0));
        let moved = renderer.render(&frame, 160, 120, 2)?;
        assert!(
            energy(&moved) > energy(&blocked),
            "moved caster left a stale shadow"
        );
        frame.draws[1].model =
            Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0)) * Mat4::from_scale(Vec3::splat(0.09));
    }
    Ok(())
}

/// The caster cull must never drop something the cone reaches — a missing
/// caster is a silently missing shadow, which no golden here is dense
/// enough to catch.
#[test]
fn cone_caster_cull_keeps_everything_the_beam_touches() {
    let apex = Vec3::ZERO;
    let direction = Vec3::Z;
    let range = 8.0;
    // cos of a 20-degree half-angle.
    let cos_field = 0.9397;
    let reaches = |centre: Vec3, radius: f32| {
        cone_reaches_sphere(apex, direction, range, cos_field, centre, radius)
    };

    // On the axis, inside the throw.
    assert!(reaches(Vec3::new(0.0, 0.0, 4.0), 0.1));
    // At the apex.
    assert!(reaches(apex, 0.1));
    // Just outside the opening angle, but a large enough sphere still
    // straddles the surface.
    let off_axis = Vec3::new(4.0 * 0.45, 0.0, 4.0);
    assert!(!reaches(off_axis, 0.05), "well outside the beam");
    assert!(reaches(off_axis, 1.5), "a sphere overlapping the beam edge");
    // Past the end of the throw, and behind the apex.
    assert!(!reaches(Vec3::new(0.0, 0.0, range + 2.0), 0.5));
    assert!(!reaches(Vec3::new(0.0, 0.0, -2.0), 0.5));
    // But a sphere big enough to reach back over the apex counts.
    assert!(reaches(Vec3::new(0.0, 0.0, -2.0), 3.0));
    // Degenerate radius must not panic or reject an axis hit.
    assert!(reaches(Vec3::new(0.0, 0.0, 1.0), 0.0));
}
