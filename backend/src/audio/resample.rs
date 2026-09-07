/// Resample stereo interleaved audio [L0, R0, L1, R1, ...] using linear interpolation.
/// Processes each channel independently to preserve stereo separation.
pub fn resample_stereo_to_target(samples: &[f32], src_rate: u32, target_rate: u32) -> Vec<f32> {
    if src_rate == 0 || target_rate == 0 || src_rate == target_rate {
        return samples.to_vec();
    }

    // Number of stereo frames
    let src_frames = samples.len() / 2;
    if src_frames == 0 {
        return Vec::new();
    }

    let ratio = target_rate as f64 / src_rate as f64;
    let new_frames = ((src_frames as f64) * ratio).ceil() as usize;
    let mut output = Vec::with_capacity(new_frames * 2);

    for i in 0..new_frames {
        let src_pos = (i as f64) / ratio;
        let lower_frame = src_pos.floor() as usize;
        let frac = (src_pos - lower_frame as f64) as f32;

        if lower_frame >= src_frames - 1 {
            // At or past the end - use last frame
            let last_idx = (src_frames - 1) * 2;
            output.push(samples[last_idx]); // L
            output.push(samples[last_idx + 1]); // R
        } else {
            // Linear interpolation for each channel independently
            let lower_idx = lower_frame * 2;
            let upper_idx = (lower_frame + 1) * 2;

            // Left channel
            let left = samples[lower_idx] * (1.0 - frac) + samples[upper_idx] * frac;
            // Right channel
            let right = samples[lower_idx + 1] * (1.0 - frac) + samples[upper_idx + 1] * frac;

            output.push(left);
            output.push(right);
        }
    }

    output
}
