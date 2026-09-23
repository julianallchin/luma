//! `luma.nodes` — the standard node definitions a score's graphs are built
//! from. Static and fully backend-loadable, so it is published in Rust.

use super::inline;
use crate::agent_execution::bindings::assembler::BindingBuilder;

pub fn provide(b: &mut BindingBuilder) -> Result<(), String> {
    inline(b, "nodes", &luma_patterns::standard_library().definitions)
}
