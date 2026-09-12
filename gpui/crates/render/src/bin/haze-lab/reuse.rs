//! CPU-only dependency survey. These exact input comparisons measure possible
//! reuse; they are deliberately not renderer cache keys or measured cache hits.
use super::{json, write_json, Path, ReplayInputs, Result};
use luma_render::frame::{EditorObject, FixtureCone};
use luma_render::Frame;
use std::collections::BTreeMap;

#[derive(PartialEq, Eq)]
struct Geometry {
    mesh: String,
    model: [u32; 16],
}

struct Inputs {
    camera: [u32; 7],
    outdoor: bool,
    bounds: [[u32; 3]; 2],
    far_uncapped: u32,
    opaque: Vec<Geometry>,
    casters: Vec<Geometry>,
    cones: Vec<Vec<u32>>,
    scattering: Vec<bool>,
    broad: Vec<bool>,
    shadow_modes: [bool; 2],
}

// Compare raw f32 bits, including signed zero. Equal raw inputs imply equal
// sanitized inputs. Unequal raw inputs can still sanitize to the same value,
// so this survey may undercount opportunities; it never uses fuzzy equality.
fn cone_geometry(cone: &FixtureCone) -> Vec<u32> {
    cone.position
        .to_array()
        .into_iter()
        .chain(cone.direction.to_array())
        .chain([
            cone.range,
            cone.cos_beam,
            cone.cos_field,
            cone.wash,
            cone.gobo_rotation,
        ])
        .map(f32::to_bits)
        .chain([cone.gobo])
        .collect()
}

impl Inputs {
    fn new(frame: &Frame) -> Result<Self> {
        // Match medium::Uniform's raw, all-cone lighting bounds. The radial
        // maximum is deliberately uncapped: equality is sufficient regardless
        // of the renderer camera-far constant, provided sky mode is unchanged.
        anyhow::ensure!(
            frame.fixture_cones.iter().all(|c| c.position.is_finite()
                && c.position.abs().max_element() <= 10_000.0
                && c.range.is_finite()
                && (0.05..=100.0).contains(&c.range)),
            "reuse survey requires finite cone positions and valid raw ranges"
        );
        let bounds = if frame.sky.is_some() {
            let bounds = luma_scene::Aabb::from_points(frame.fixture_cones.iter().flat_map(|c| {
                let radius = glam::Vec3::splat(c.range);
                [c.position - radius, c.position + radius]
            }));
            if bounds.is_empty() {
                luma_scene::Aabb::new(glam::Vec3::ZERO, glam::Vec3::ZERO)
            } else {
                bounds
            }
        } else {
            frame.haze_bounds
        };
        let geometry = |draw: &luma_render::frame::Draw| Geometry {
            mesh: frame.meshes[draw.mesh].key.clone(),
            model: draw.model.to_cols_array().map(f32::to_bits),
        };
        let opaque = &frame.draws[..frame.draws.len() - frame.transparent.len()];
        let cones = &frame.fixture_cones[..frame
            .fixture_cones
            .len()
            .min(luma_render::frame::MAX_FIXTURE_CONES)];
        Ok(Self {
            camera: [
                frame.camera.eye.x,
                frame.camera.eye.y,
                frame.camera.eye.z,
                frame.camera.target.x,
                frame.camera.target.y,
                frame.camera.target.z,
                frame.camera.fov_y_deg,
            ]
            .map(f32::to_bits),
            outdoor: frame.sky.is_some(),
            bounds: [bounds.min.to_array(), bounds.max.to_array()]
                .map(|point| point.map(f32::to_bits)),
            far_uncapped: cones
                .iter()
                .map(|c| frame.camera.eye.distance(c.position) + c.range)
                .fold(1.0_f32, f32::max)
                .to_bits(),
            opaque: opaque.iter().map(geometry).collect(),
            casters: opaque
                .iter()
                .filter(|d| !matches!(&d.editor_object, Some(EditorObject::Fixture(_))))
                .map(geometry)
                .collect(),
            cones: cones.iter().map(cone_geometry).collect(),
            scattering: cones.iter().map(|c| c.haze_gain > 0.0).collect(),
            broad: cones
                .iter()
                .map(|c| c.wash >= 0.65 && c.gobo == 0)
                .collect(),
            shadow_modes: [frame.fixture_shadows, frame.geometry_shadows],
        })
    }

    fn compare(&self, previous: &Self) -> serde_json::Value {
        let camera = self.camera == previous.camera && self.outdoor == previous.outdoor;
        let grid_positions =
            camera && self.bounds == previous.bounds && self.far_uncapped == previous.far_uncapped;
        let casters = self.casters == previous.casters;
        let mut available = BTreeMap::<&Vec<u32>, Vec<usize>>::new();
        for (index, cone) in previous.cones.iter().enumerate() {
            available.entry(cone).or_default().push(index);
        }
        // A multiset match avoids compacted/sorted ids pretending to be stable
        // identities, while also avoiding double-counting coincident emitters.
        let matches: Vec<_> = self
            .cones
            .iter()
            .map(|c| available.get_mut(c).and_then(Vec::pop))
            .collect();
        let matched_scattering = matches
            .iter()
            .enumerate()
            .filter(|(i, old)| self.scattering[*i] && old.is_some_and(|j| previous.scattering[j]));
        let matched_broad = matched_scattering
            .clone()
            .filter(|(i, old)| self.broad[*i] && previous.broad[old.unwrap()])
            .count();
        json!({
            "camera_identical":camera,
            "lighting_bounds_identical":self.bounds == previous.bounds,
            "fog_far_uncapped_identical":self.far_uncapped == previous.far_uncapped,
            "grid_sample_positions_identical":grid_positions,
            "opaque_depth_inputs_identical":self.opaque == previous.opaque,
            "fixture_caster_inputs_identical":casters,
            "shadow_modes_identical":self.shadow_modes == previous.shadow_modes,
            "cone_geometry_matches":matches.iter().filter(|v| v.is_some()).count(),
            "same_source_index_geometry_matches":self.cones.iter().zip(&previous.cones).filter(|(a,b)| a == b).count(),
            "scattering_geometry_matches":matched_scattering.clone().count(),
            "broad_scattering_geometry_matches":matched_broad,
            "conservative_native_visibility_reuse_candidates":if camera && casters
                && self.opaque == previous.opaque && self.shadow_modes == previous.shadow_modes {
                    matched_scattering.count()
                } else { 0 },
            "conservative_grid_visibility_reuse_candidates":if grid_positions && casters
                && self.shadow_modes == previous.shadow_modes { matched_broad } else { 0 },
            "current_to_previous_geometry":matches,
        })
    }
}

pub(super) fn inspect(suite: &Path, output: &Path) -> Result<()> {
    let inputs = ReplayInputs::load(suite)?;
    let states: Vec<_> = inputs
        .frames
        .iter()
        .map(|(frame, _)| Inputs::new(frame))
        .collect::<Result<_>>()?;
    inputs.archive(output)?;
    let rows: Vec<_> = states.iter().enumerate().map(|(index, state)| json!({
        "case":inputs.suite.cases[index],
        "build_ms":inputs.frames[index].1,
        "cones":state.cones.len(),
        "scattering_cones":state.scattering.iter().filter(|v| **v).count(),
        "broad_scattering_cones":state.scattering.iter().zip(&state.broad).filter(|(a,b)| **a && **b).count(),
        "opaque_draws":state.opaque.len(),
        "caster_draws":state.casters.len(),
        "lighting_bounds_bits":state.bounds,
        "lighting_bounds":state.bounds.map(|point| point.map(f32::from_bits)),
        "fog_far_uncapped_bits":state.far_uncapped,
        "fog_far_uncapped":f32::from_bits(state.far_uncapped),
        "previous":index.checked_sub(1).map(|i| state.compare(&states[i])),
        "first":state.compare(&states[0]),
    })).collect();
    write_json(
        &output.join("reuse.json"),
        &json!({
            "size":[inputs.suite.width,inputs.suite.height],
            "gpu_used":false,
            "note":"CPU-only exact input survey, not measured GPU cache hits or FPS. Uses mesh identity's immutable-geometry contract and preserves caster/depth draw order. Grid sample positions exclude depth-prefix activity: changing camera depth can expose previously uncomputed cells. A cache must track valid cells and shadow-map contents, remap light identities, and preserve current sorted addition order. Density, time, colour and intensity are deliberately excluded from geometric visibility; radiance must still be recomputed. Environment overrides and actual shadow-slot residency are not simulated.",
            "frames":rows,
        }),
    )?;
    println!("inspected {} frames without a GPU", rows.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        let catalogue = luma_render::Catalogue::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("experiments/haze-shadow.json"),
        )
        .unwrap();
        luma_render::build_frame(
            &catalogue.scenes[0],
            &catalogue.definitions,
            1.0,
            &mut crate::library(),
        )
        .unwrap()
    }

    #[test]
    fn visibility_survey_separates_radiance_camera_and_grid_dependencies() {
        let mut frame = frame();
        let original = Inputs::new(&frame).unwrap();
        assert!(!original.cones.is_empty());
        frame.time += 0.013333;
        frame.haze_density *= 0.5;
        frame.haze_appearance.cloud_size *= 2.0;
        for cone in &mut frame.fixture_cones {
            cone.color *= 0.25;
            cone.intensity *= 0.5;
        }
        let comparison = Inputs::new(&frame).unwrap().compare(&original);
        assert_eq!(
            comparison["conservative_native_visibility_reuse_candidates"],
            original.cones.len()
        );
        assert_eq!(comparison["grid_sample_positions_identical"], true);
        frame.camera.eye.x = frame.camera.eye.x.next_up();
        let comparison = Inputs::new(&frame).unwrap().compare(&original);
        assert_eq!(comparison["camera_identical"], false);
        assert_eq!(
            comparison["conservative_native_visibility_reuse_candidates"],
            0
        );

        let mut frame = self::frame();
        frame.sky = None;
        let original = Inputs::new(&frame).unwrap();
        frame.haze_bounds.max.x += 0.01;
        let comparison = Inputs::new(&frame).unwrap().compare(&original);
        assert_eq!(comparison["grid_sample_positions_identical"], false);
        assert_eq!(
            comparison["conservative_native_visibility_reuse_candidates"],
            original.cones.len()
        );
    }

    #[test]
    fn moving_fixture_depth_and_stage_shadow_casters_are_distinct() {
        let mut frame = frame();
        let original = Inputs::new(&frame).unwrap();
        let draw = frame
            .draws
            .iter_mut()
            .find(|draw| matches!(&draw.editor_object, Some(EditorObject::Fixture(_))))
            .unwrap();
        draw.model.w_axis.x += 0.01;
        let comparison = Inputs::new(&frame).unwrap().compare(&original);
        assert_eq!(comparison["opaque_depth_inputs_identical"], false);
        assert_eq!(comparison["fixture_caster_inputs_identical"], true);
        assert_eq!(
            comparison["conservative_native_visibility_reuse_candidates"],
            0
        );
        let draw = frame
            .draws
            .iter_mut()
            .find(|draw| matches!(&draw.editor_object, Some(EditorObject::StagePiece(_))))
            .unwrap();
        draw.model.w_axis.x += 0.01;
        let comparison = Inputs::new(&frame).unwrap().compare(&original);
        assert_eq!(comparison["fixture_caster_inputs_identical"], false);
        assert_eq!(
            comparison["conservative_grid_visibility_reuse_candidates"],
            0
        );
    }

    #[test]
    fn compacted_light_matches_are_a_multiset_and_preserve_single_bit_changes() {
        let mut frame = frame();
        let light = frame.fixture_cones[0];
        let mut other = light;
        other.position.x += 1.0;
        frame.fixture_cones = vec![light, light, other];
        let original = Inputs::new(&frame).unwrap();
        frame.fixture_cones.reverse();
        let comparison = Inputs::new(&frame).unwrap().compare(&original);
        assert_eq!(comparison["cone_geometry_matches"], 3);
        assert_eq!(comparison["same_source_index_geometry_matches"], 1);
        frame.fixture_cones[0] = light;
        let comparison = Inputs::new(&frame).unwrap().compare(&original);
        assert_eq!(comparison["cone_geometry_matches"], 2);
        frame.fixture_cones[0].position.x = light.position.x.next_up();
        frame.fixture_cones[1].direction.y = light.direction.y.next_up();
        let comparison = Inputs::new(&frame).unwrap().compare(&original);
        assert_eq!(comparison["cone_geometry_matches"], 1);
    }
}
