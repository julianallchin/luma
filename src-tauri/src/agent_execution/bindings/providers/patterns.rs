//! `luma.patterns` — the pattern pool the agent can reference, with the argument
//! schema of each one.
//!
//! Both halves are fully backend-loadable (patterns + implementations live in
//! `luma.db`), so they are published in Rust rather than shipped over the bridge
//! from the frontend. Only the *unsaved* graph-editor buffer is frontend-owned;
//! see `graph.rs`.

use std::collections::BTreeMap;

use serde::Serialize;

use super::{inline, ProviderCtx};
use crate::agent_execution::bindings::assembler::BindingBuilder;
use crate::database::local;

/// Above this many patterns the argument schemas stop being a bounded payload;
/// the summaries are still all published, and the note says which are missing.
const ARG_SCHEMA_LIMIT: usize = 200;

#[derive(Serialize)]
struct PatternBinding {
    id: String,
    name: String,
    description: Option<String>,
    category: Option<String>,
    is_verified: bool,
}

pub async fn provide(b: &mut BindingBuilder, ctx: &ProviderCtx<'_>) -> Result<(), String> {
    let patterns = match local::patterns::list_patterns_pool(ctx.pool).await {
        Ok(p) => p,
        Err(e) => {
            super::unavailable(
                b,
                "patterns",
                format!("the pattern pool could not be loaded: {e}"),
            )?;
            return Ok(());
        }
    };

    let summaries: Vec<PatternBinding> = patterns
        .iter()
        .map(|p| PatternBinding {
            id: p.id.clone(),
            name: p.name.clone(),
            description: p.description.clone(),
            category: p.category_name.clone(),
            is_verified: p.is_verified,
        })
        .collect();
    inline(b, "patterns.summaries", &summaries)?;

    // Arg schemas parse each pattern's graph JSON, so they are the expensive
    // half. Truncate rather than blow up a manifest on a huge pool.
    let mut schemas: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for pattern in patterns.iter().take(ARG_SCHEMA_LIMIT) {
        if let Ok(args) = local::patterns::get_pattern_args_pool(ctx.pool, &pattern.id).await {
            if let Ok(value) = serde_json::to_value(&args) {
                schemas.insert(pattern.id.clone(), value);
            }
        }
    }
    inline(b, "patterns.argument_schemas", &schemas)?;
    inline(
        b,
        "patterns.note",
        if patterns.len() > ARG_SCHEMA_LIMIT {
            format!(
                "{} patterns; argument schemas are included for the {ARG_SCHEMA_LIMIT} \
                 most recently updated only",
                patterns.len()
            )
        } else {
            format!("{} patterns, all with argument schemas", patterns.len())
        },
    )
}
