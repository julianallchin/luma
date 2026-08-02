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
use crate::services::graph_documents::{
    load_graph_document_unscoped, resolve_graph_implementation,
};

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

    // Graph validation is the expensive half. Truncate rather than blow up a
    // manifest on a huge pool. Invalid stored graphs are reported explicitly:
    // an absent schema must never look like a pattern with zero arguments.
    let mut schemas: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut schema_errors: BTreeMap<String, String> = BTreeMap::new();
    for pattern in patterns.iter().take(ARG_SCHEMA_LIMIT) {
        let resolved = resolve_graph_implementation(
            ctx.pool,
            &pattern.id,
            ctx.scope.venue_id.as_deref(),
            None,
        )
        .await;
        let document = match resolved {
            Ok(implementation_id) => {
                load_graph_document_unscoped(ctx.pool, &pattern.id, &implementation_id).await
            }
            Err(error) => Err(error),
        };
        match document {
            Ok(document) => match serde_json::to_value(&document.graph.args) {
                Ok(value) => {
                    schemas.insert(pattern.id.clone(), value);
                }
                Err(error) => {
                    schema_errors.insert(pattern.id.clone(), error.to_string());
                }
            },
            Err(error) => {
                schema_errors.insert(pattern.id.clone(), error.to_string());
            }
        }
    }
    inline(b, "patterns.argument_schemas", &schemas)?;
    inline(b, "patterns.argument_schema_errors", &schema_errors)?;
    inline(
        b,
        "patterns.note",
        if patterns.len() > ARG_SCHEMA_LIMIT {
            format!(
                "{} patterns; argument schemas are included for the {ARG_SCHEMA_LIMIT} \
                 most recently updated only; {} included graph(s) were invalid",
                patterns.len(),
                schema_errors.len()
            )
        } else {
            format!(
                "{} patterns; {} argument schema(s) available, {} invalid",
                patterns.len(),
                schemas.len(),
                schema_errors.len()
            )
        },
    )
}
