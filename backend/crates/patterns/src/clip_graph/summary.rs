//! The one-line summary of a graph (spec 7.6), such as `line · every 2`.
use super::{ClipGraph, Input, Kind};

impl ClipGraph {
    /// Parts joined by " · ": each space's kind word, each time node's
    /// `every {E}` (or `every varies`), `noise` when present, `audio
    /// {lo}–{hi} Hz` when present; `still` when none apply. A part that
    /// repeats is written once.
    pub fn summary(&self) -> String {
        let ids = self.ids_in_order();
        let nodes = |kind: Kind| {
            ids.iter()
                .map(|id| &self.nodes[*id])
                .filter(move |node| node.kind == kind)
        };
        let mut parts: Vec<String> = Vec::new();
        for node in nodes(Kind::Space) {
            parts.push(node.setting("kind").unwrap_or("line").to_string());
        }
        for node in nodes(Kind::Time) {
            parts.push(
                match node.inputs.get("every").map(|every| self.resolve(every)) {
                    None => continue,
                    Some(Input::Number(every)) => format!("every {every}"),
                    _ => "every varies".into(),
                },
            );
        }
        if nodes(Kind::Noise).next().is_some() {
            parts.push("noise".into());
        }
        for node in nodes(Kind::Audio) {
            let hz = |name: &str, empty: f64| match node.inputs.get(name).map(|hz| self.resolve(hz))
            {
                Some(Input::Number(hz)) => Some(*hz),
                None => Some(empty),
                _ => None,
            };
            let (empty_low, empty_high) = crate::presets::empty_band();
            parts.push(match (hz("low_hz", empty_low), hz("high_hz", empty_high)) {
                (Some(low), Some(high)) => format!("audio {low}–{high} Hz"),
                _ => "audio".into(),
            });
        }
        let mut seen = std::collections::HashSet::new();
        parts.retain(|part| seen.insert(part.clone()));
        if parts.is_empty() {
            return "still".into();
        }
        parts.join(" · ")
    }
}
