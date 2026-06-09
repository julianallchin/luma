//! Lowering for audio-reactive nodes. Owns: frequency_amplitude, stem_splitter
//! (drum_events / harmony_analysis still unclaimed — no ops yet). These map to
//! `AudioOp` variants and read `ResidentContext.audio` / `.stems` (the upstream
//! `audio_input` node is a no-op; audio ops take the spectrum from the resident
//! buffer, not an input slot). Reference: `eval/ops/audio.rs`.

use super::{CompileError, LowerCtx, Lowerer};
use crate::eval::ops::audio::AudioOp;
use crate::eval::{OpKind, Phase};
use serde_json::Value;

pub fn lower_audio(lc: &LowerCtx, low: &mut Lowerer) -> Option<Result<(), CompileError>> {
    if !matches!(
        lc.type_id(),
        "frequency_amplitude" | "stem_splitter" | "drum_events" | "harmony_analysis"
    ) {
        return None;
    }
    Some(go(lc, low))
}

fn go(lc: &LowerCtx, low: &mut Lowerer) -> Result<(), CompileError> {
    let id = &lc.node.id;
    match lc.type_id() {
        "frequency_amplitude" => {
            let ranges = parse_ranges(lc.param("selected_frequency_ranges"));
            // If audio_in traces back to a stem_splitter, analyze that stem.
            let stem = stem_source(lc, "audio_in");
            low.emit(
                OpKind::Audio(AudioOp::FreqAmplitude { ranges, stem }),
                vec![],
                1,
                1,
                Phase::Kernel,
                id,
                lc.out_port(),
            );
        }
        // stem_splitter selects a preprocessed stem; it carries no compute of its
        // own. It lowers to nothing — downstream audio ops trace through it to the
        // stem name (see `stem_source`) and read `ResidentContext.stems`.
        "stem_splitter" => {}
        // drum_events emits onset times per class on its `<class>_out` ports; it
        // lowers to nothing — adsr / random_select_mask trace `events_in` back to
        // it (see LowerCtx::event_pulses) and bake the onsets.
        "drum_events" => {}
        // harmony_analysis emits a one-hot 12-channel chroma signal from the
        // track's chord sections (ResidentContext.chord_sections).
        "harmony_analysis" => {
            low.emit(
                OpKind::Audio(AudioOp::Chroma),
                vec![],
                1,
                12,
                Phase::Kernel,
                id,
                lc.out_port(),
            );
        }
        _ => unreachable!("claimed type not handled"),
    }
    Ok(())
}

/// If `port` is fed by a `stem_splitter` output, return the stem name
/// (`bass_out` → `bass`, etc); else `None` (analyze the full mix).
fn stem_source(lc: &LowerCtx, port: &str) -> Option<String> {
    let e = lc.edge_to(port)?;
    let src = lc.by_id.get(e.from_node.as_str())?;
    if src.type_id != "stem_splitter" {
        return None;
    }
    e.from_port.strip_suffix("_out").map(str::to_string)
}

/// Parse `selected_frequency_ranges` (a JSON string like `"[[20,60]]"` or an
/// inline array) into `Vec<[f32;2]>`. Empty/invalid -> a full-band default.
fn parse_ranges(v: Option<&Value>) -> Vec<[f32; 2]> {
    let owned;
    let arr = match v {
        Some(Value::String(s)) => {
            owned = serde_json::from_str::<Value>(s).unwrap_or(Value::Null);
            owned.as_array().cloned()
        }
        Some(Value::Array(a)) => Some(a.clone()),
        _ => None,
    };
    let ranges: Vec<[f32; 2]> = arr
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            let pair = r.as_array()?;
            Some([
                pair.first()?.as_f64()? as f32,
                pair.get(1)?.as_f64()? as f32,
            ])
        })
        .collect();
    if ranges.is_empty() {
        vec![[20.0, 20_000.0]]
    } else {
        ranges
    }
}
