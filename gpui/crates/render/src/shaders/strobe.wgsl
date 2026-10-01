// A rolling shutter's strobe (`strobe.rs`, `Rows::ratio`): row `y`'s share
// of the light the frame's cone intensity already holds, 0 at the top row and
// 1 at the bottom. In periods of the flash train: the row sees `[phase + y *
// readout, ... + span]`, a flash is the first `duty` of each period.
// `norm == 0` is a light every row sees alike, and the ratio is then exactly
// 1, so multiplying by it changes no bit of a steady light.
fn strobe_on_time(x: f32, duty: f32) -> f32 {
    let whole = floor(x);
    return whole * duty + min(x - whole, duty);
}

fn strobe_row_ratio(phase: f32, span: f32, readout: f32, duty: f32, norm: f32, y: f32) -> f32 {
    if norm <= 0.0 {
        return 1.0;
    }
    let a = phase + clamp(y, 0.0, 1.0) * readout;
    return clamp((strobe_on_time(a + span, duty) - strobe_on_time(a, duty)) * norm, 0.0, 1.0);
}
