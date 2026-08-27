// Fixture-light photometry shared by opaque surface lighting and volumetric
// transport. This source deliberately declares no resources or bind groups so
// either pass can prepend it without coupling their GPU layouts.

/// Compare reference for a fixture-shadow lookup with *metric* slack: the
/// sample only reads as occluded when the stored caster is more than `slack`
/// metres in front of it. Reverse-Z projective depth is metrically non-uniform
/// — a constant raw bias is centimetres near the light and metres at range,
/// which reads as the beam spilling straight through occluders (fog samples a
/// metre behind a tabletop still passed). Linearise the sample depth with the
/// projection's own planes, subtract the slack, and re-project; the raw
/// comparison against the map (hardware or manual) is unchanged.
fn shadow_compare_reference(raw_z: f32, near: f32, far: f32, slack: f32) -> f32 {
    let sample_depth = near * far / max(near + raw_z * (far - near), 1e-5);
    let reference_depth = max(sample_depth - slack, near);
    return clamp(
        near * (far - reference_depth) / max(reference_depth * (far - near), 1e-5),
        0.0,
        1.0,
    );
}

/// Peaked photometric profile with GDTF beam/field semantics: 100% on the
/// axis, 50% at the beam angle, smoothly cut to zero approaching the field
/// angle.
fn angular_profile(cos_angle: f32, cos_beam: f32, cos_field: f32) -> f32 {
    if cos_angle <= cos_field {
        return 0.0;
    }
    // (1-cos) scales as angle squared, so this ratio is
    // (theta/thetaBeam)^2. exp(-ln(2)*t) puts the 50% point at the beam angle.
    let t = (1.0 - cos_angle) / max(1.0 - cos_beam, 1e-5);
    let peak = exp(-0.6931472 * t);
    let cut = smoothstep(cos_field, mix(cos_field, cos_beam, 0.35), cos_angle);
    return peak * cut;
}

/// Procedural aperture shared by a fixture's visible pool and its haze cone.
/// `q` points from the lens to the sample in world space.
fn gobo_transmission(
    q: vec3<f32>,
    direction: vec3<f32>,
    cos_field: f32,
    gobo: f32,
    gobo_rotation: f32,
) -> f32 {
    if gobo < 0.5 {
        return 1.0;
    }
    let helper = select(
        vec3<f32>(0.0, 0.0, 1.0),
        vec3<f32>(0.0, 1.0, 0.0),
        abs(direction.z) > 0.98,
    );
    let right = normalize(cross(direction, helper));
    let up = cross(right, direction);
    let axial = max(dot(q, direction), 1e-4);
    let field_sine = sqrt(max(1.0 - cos_field * cos_field, 0.0));
    let field_radius = axial * field_sine / max(abs(cos_field), 0.05);
    let aperture = vec2<f32>(dot(q, right), dot(q, up)) / max(field_radius, 1e-4);
    if gobo < 1.5 {
        let angle = atan2(aperture.y, aperture.x) + gobo_rotation;
        return smoothstep(0.18, 0.48, abs(cos(angle * 6.0)));
    }
    let c = cos(gobo_rotation);
    let s = sin(gobo_rotation);
    let rotated = mat2x2<f32>(vec2<f32>(c, s), vec2<f32>(-s, c)) * aperture;
    let breakup = sin(rotated.x * 15.0) * sin(rotated.y * 11.0);
    return smoothstep(-0.15, 0.25, breakup);
}
