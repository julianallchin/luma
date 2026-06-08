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
    if !matches!(lc.type_id(), "frequency_amplitude" | "stem_splitter") {
        return None;
    }
    Some(go(lc, low))
}

fn go(lc: &LowerCtx, low: &mut Lowerer) -> Result<(), CompileError> {
    let id = &lc.node.id;
    match lc.type_id() {
        "frequency_amplitude" => {
            let ranges = parse_ranges(lc.param("selected_frequency_ranges"));
            low.emit(OpKind::Audio(AudioOp::FreqAmplitude { ranges }), vec![], 1, 1, Phase::Kernel, id, lc.out_port());
        }
        "stem_splitter" => {
            // Stub: reads ResidentContext.stems which the runner doesn't populate
            // yet (returns zeros). One op, registered on every stem output port.
            let stem = lc.param_str("stem").unwrap_or_else(|| "drums".to_string());
            let out = low.alloc(1, 1);
            low.ops.push(crate::eval::Op {
                kind: OpKind::Audio(AudioOp::StemSplit { stem }),
                inputs: vec![],
                out,
                phase: Phase::Kernel,
            });
            for port in lc.out_ports() {
                low.node_slot.insert((id.clone(), port.clone()), out);
            }
        }
        _ => unreachable!("claimed type not handled"),
    }
    Ok(())
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
