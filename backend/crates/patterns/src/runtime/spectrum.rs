use crate::*;
use ndarray::Array3;

/// Bind the frequency axis once, then collect immutable samples along time.
/// A provider cannot silently change channel meaning within one signal.
pub(super) fn sample(
    source: &dyn FeatureSource,
    request: &FeatureRequest,
    beats: &[f64],
    clip_start: f64,
) -> Result<(Signal, Signal)> {
    let FeatureSample::Spectrum {
        bins: first,
        bin_hz,
    } = source.sample(request, beats.first().copied().unwrap_or(clip_start))?
    else {
        return Err(Error(
            "track source returned the wrong spectrum feature".into(),
        ));
    };
    let channels = Channels::components(first.len())?;
    if !bin_hz.is_finite() || bin_hz <= 0. {
        return Err(Error(
            "spectrum frequency spacing must be positive and finite".into(),
        ));
    }
    let width = first.len();
    validate_bins(&first)?;
    let mut values = if beats.is_empty() { Vec::new() } else { first };
    values.reserve(beats.len().saturating_sub(1) * width);
    for beat in beats.iter().skip(1) {
        let FeatureSample::Spectrum { bins, bin_hz: step } = source.sample(request, *beat)? else {
            return Err(Error(
                "track source returned the wrong spectrum feature".into(),
            ));
        };
        if bins.len() != width || step != bin_hz {
            return Err(Error(
                "spectrum frequency axis changed within a batch".into(),
            ));
        }
        validate_bins(&bins)?;
        values.extend(bins);
    }
    Ok((
        Signal::new(
            Array3::from_shape_vec((1, beats.len(), width), values).unwrap(),
            Unit::Number,
            channels,
            None,
        )?,
        Signal::scalar(bin_hz, Unit::Number)?,
    ))
}

fn validate_bins(bins: &[f64]) -> Result<()> {
    if bins.iter().any(|v| !v.is_finite() || *v < 0.) {
        return Err(Error(
            "spectrum magnitudes must be finite and nonnegative".into(),
        ));
    }
    Ok(())
}
