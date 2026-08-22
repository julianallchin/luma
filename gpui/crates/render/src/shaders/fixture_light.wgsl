// Fixture-light photometry shared by opaque surface lighting and volumetric
// transport. This source deliberately declares no resources or bind groups so
// either pass can prepend it without coupling their GPU layouts.

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
