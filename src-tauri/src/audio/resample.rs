pub fn resample_to_target(samples: &[f32], src_rate: u32, target_rate: u32) -> Vec<f32> {
    if src_rate == 0 || target_rate == 0 || src_rate == target_rate {
        return samples.to_vec();
    }

    let ratio = target_rate as f64 / src_rate as f64;
    let new_len = ((samples.len() as f64) * ratio).ceil() as usize;
    let mut output = Vec::with_capacity(new_len.max(1));

    for i in 0..new_len {
        let src_pos = (i as f64) / ratio;
        let lower = src_pos.floor() as usize;
        if lower >= samples.len() - 1 {
            output.push(*samples.last().unwrap_or(&0.0));
        } else {
            let frac = src_pos - lower as f64;
            let lower_val = samples[lower];
            let upper_val = samples[lower + 1];
            let val = lower_val * (1.0 - frac as f32) + upper_val * frac as f32;
            output.push(val);
        }
    }

    output
}
