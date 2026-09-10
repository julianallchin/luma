//! Audio sources remain structured controls; spectra and band weighting are
//! ordinary numerical signals, shared between all consumers of one spectrum.
use super::*;

pub(super) fn lower(node: &NodeInstance) -> Result<Definition, String> {
    let mut b = Builder::default();
    if matches!(node.type_id.as_str(), "lowpass_filter" | "highpass_filter") {
        let source = b.input("audio_in", "Audio source", ValueType::AudioSource, None);
        let cutoff = b.scalar(node, "cutoff_hz", "Cutoff (Hz)", 200.);
        for input in b.inputs.values_mut() {
            input.rate = Rate::Fixed;
        }
        let cutoff = b.math("maximum", cutoff, number(1.));
        let highpass = node.type_id == "highpass_filter";
        let filtered = b.call(
            if highpass {
                "audio_highpass"
            } else {
                "audio_lowpass"
            },
            [("source", source), ("cutoff_hz", cutoff)],
            "source",
        );
        return b.finish(
            if highpass {
                "Audio highpass"
            } else {
                "Audio lowpass"
            },
            [("audio_out", filtered)],
        );
    }
    if node.type_id == "stem_splitter" {
        return b.finish(
            "Audio sources",
            [
                (
                    "bass_out",
                    Value::AudioSource(p::AudioSource::Bass.into()).into(),
                ),
                (
                    "drums_out",
                    Value::AudioSource(p::AudioSource::Drums.into()).into(),
                ),
                (
                    "vocals_out",
                    Value::AudioSource(p::AudioSource::Vocals.into()).into(),
                ),
                (
                    "other_out",
                    Value::AudioSource(p::AudioSource::Other.into()).into(),
                ),
            ],
        );
    }
    let source = b.input(
        "audio_in",
        "Audio source",
        ValueType::AudioSource,
        Some(Value::AudioSource(p::AudioSource::Mix.into())),
    );
    let hold = b.input(
        "hold_edges",
        "Hold audio boundaries",
        ValueType::Boolean,
        Some(Value::Boolean(true)),
    );
    for key in ["audio_in", "hold_edges"] {
        b.inputs.get_mut(key).unwrap().rate = Rate::Fixed;
    }
    let raw = node
        .params
        .get("selected_frequency_ranges")
        .cloned()
        .map(parse_json)
        .unwrap_or_default();
    let mut ranges: Vec<[f64; 2]> = raw
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let pair = r.as_array()?;
            Some([
                f64::from(pair.first()?.as_f64()? as f32),
                f64::from(pair.get(1)?.as_f64()? as f32),
            ])
        })
        .collect();
    if ranges.is_empty() {
        ranges.push([20., 20_000.]);
    }
    let bounds = p::Signal::vector(ranges.iter().flatten().copied().collect(), p::Unit::Number)
        .map_err(|e| e.to_string())?;
    let kind = ValueType::Signal(p::SignalType::new(bounds.unit(), *bounds.channels()));
    let bounds = b.input(
        "ranges",
        "Frequency ranges (Hz)",
        kind,
        Some(Value::Signal(bounds)),
    );
    b.inputs.get_mut("ranges").unwrap().rate = Rate::Fixed;
    let bounds = b.unary("float32", bounds);
    let spectrum = b.node("audio_spectrum", [("source", source), ("hold_edges", hold)]);
    let bins = wire(&spectrum, "spectrum");
    let step = wire(&spectrum, "bin_hz");
    let index = b.call("core/channel_index", [("value", bins.clone())], "value");
    let count = b.call("core/channel_count", [("value", bins.clone())], "value");
    let last = b.math("subtract", count, number(1.));
    let mut weights = Vec::new();
    for range in 0..ranges.len() {
        let low = b.channel_at(bounds.clone(), range * 2);
        let high = b.channel_at(bounds.clone(), range * 2 + 1);
        let low = b.math("divide", low, step.clone());
        // The original FFT bin selection rounded the quotient before floor or
        // ceil. At fractional bin boundaries a neighboring bin can differ.
        let low = b.unary("float32", low);
        let low = b.unary("floor", low);
        let low = b.math("maximum", low, number(0.));
        let low = b.math("minimum", low, last.clone());
        let high = b.math("divide", high, step.clone());
        let high = b.unary("float32", high);
        let high = b.ceil(high);
        let high = b.math("minimum", high, last.clone());
        let high = b.math("maximum", high, low.clone());
        let below = b.greater(low, index.clone());
        let above = b.greater(index.clone(), high);
        let included = b.math("add", below, above);
        weights.push(b.math("subtract", number(1.), included));
    }
    while weights.len() > 1 {
        weights = weights
            .chunks(2)
            .map(|pair| {
                if pair.len() == 2 {
                    b.math("add", pair[0].clone(), pair[1].clone())
                } else {
                    pair[0].clone()
                }
            })
            .collect();
    }
    let weights = weights.pop().unwrap();
    let count = b.call("core/channel_sum", [("value", weights.clone())], "value");
    let weighted = b.math("multiply", bins, weights);
    let sum = b.call("core/channel_sum", [("value", weighted)], "value");
    let amplitude = b.math("divide", sum, count);
    b.finish("Frequency amplitude", [("amplitude_out", amplitude)])
}
