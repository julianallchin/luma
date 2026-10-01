//! `luma.nodes` — the clip-graph node definitions, one record per kind — and
//! `luma.presets` — the shipped clip presets, curves, gradients and bands.
//! Static and fully backend-loadable, so they are published in Rust.

use super::inline;
use crate::agent_execution::bindings::assembler::BindingBuilder;

pub fn provide(b: &mut BindingBuilder) -> Result<(), String> {
    inline(b, "nodes", luma_patterns::clip_graph::definitions())?;
    inline(b, "presets", luma_patterns::presets())
}
