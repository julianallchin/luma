use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::local::patterns as db;
use crate::database::remote::common::SupabaseClient;
use crate::database::remote::queries as remote_queries;
use crate::dispatch::{AppServices, CommandError};
use crate::models::authored_state::AuthoredProjectedDocument;
use crate::models::node_graph::{Graph, PatternArgDef};
use crate::models::patterns::{ForkPatternInput, ForkPatternResult, PatternSummary};
use crate::services::graph_documents::{
    load_visible_graph_document, GraphDocument, GraphEditResult,
};

pub async fn list_patterns(services: &AppServices) -> Result<Vec<PatternSummary>, CommandError> {
    Ok(db::list_patterns_pool(&services.db.0).await?)
}

/// Errors rather than returning `None` for an unknown id — callers treat a
/// pattern id as a resolved reference, not a lookup that may miss.
pub async fn get_pattern(
    services: &AppServices,
    id: String,
) -> Result<PatternSummary, CommandError> {
    Ok(db::get_pattern_pool(&services.db.0, &id).await?)
}

/// Idempotent on `request_id`: replaying a request returns the same pattern
/// rather than creating a second one.
pub async fn create_pattern(
    services: &AppServices,
    request_id: String,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, CommandError> {
    let uid = services.session_user_id().await?;
    let pattern = services
        .authored
        .create_pattern(
            &services.db.0,
            uid.as_deref(),
            &request_id,
            name,
            description,
        )
        .await?;
    services.sync.push_notify.notify_one();
    Ok(pattern)
}

/// A full replace of both metadata fields, not a patch. Unlike create/delete
/// this writes the SQLite projection directly: pattern metadata is not part of
/// the authored Git document.
pub async fn update_pattern(
    services: &AppServices,
    id: String,
    name: String,
    description: Option<String>,
) -> Result<PatternSummary, CommandError> {
    let pattern = db::update_pattern_pool(&services.db.0, &id, name, description).await?;
    services.sync.push_notify.notify_one();
    Ok(pattern)
}

pub async fn fork_pattern(
    services: &AppServices,
    input: ForkPatternInput,
) -> Result<ForkPatternResult, CommandError> {
    let uid = services.session_user_id().await?;
    let result = services
        .authored
        .fork_pattern(&services.db.0, uid.as_deref(), input)
        .await?;
    services.sync.push_notify.notify_one();
    Ok(result)
}

/// Archive, not hard delete. Ownership is enforced inside `archive_pattern`,
/// which is the layer that owns the invariant.
pub async fn delete_pattern(services: &AppServices, id: String) -> Result<(), CommandError> {
    let principal = services.session_user_id().await?;
    services
        .authored
        .archive_pattern(&services.db.0, principal.as_deref(), &id)
        .await?;
    services.sync.push_notify.notify_one();
    Ok(())
}

/// Does not notify the sync engine: a category change rides along with the next
/// write that does.
pub async fn set_pattern_category(
    services: &AppServices,
    pattern_id: String,
    category_name: Option<String>,
) -> Result<(), CommandError> {
    Ok(
        db::set_pattern_category_pool(&services.db.0, &pattern_id, category_name.as_deref())
            .await?,
    )
}

/// Resolves the visible implementation venue-agnostically (`venue_id = None`),
/// unlike `get_pattern_args`.
pub async fn get_pattern_graph_document(
    services: &AppServices,
    id: String,
    implementation_id: Option<String>,
) -> Result<GraphDocument, CommandError> {
    let pool = &services.db.0;
    db::get_pattern_pool(pool, &id).await?;
    Ok(load_visible_graph_document(pool, &id, None, implementation_id.as_deref()).await?)
}

/// Verifying also stamps the author's display name — the two are one write in
/// the UI's model. The push is synchronous rather than a `notify_one` nudge so
/// other users see the verified state immediately; that also makes a network
/// failure surface *after* the local writes landed.
///
/// Reads the session's access token directly rather than through
/// [`AppServices::session_user_id`], which yields only a principal.
pub async fn verify_pattern(
    services: &AppServices,
    id: String,
    verify: bool,
) -> Result<PatternSummary, CommandError> {
    let pool = &services.db.0;
    let auth = auth::get_current_auth(&services.state_db.0)
        .await?
        .ok_or_else(|| CommandError::Unauthorized("Not authenticated".to_string()))?;
    let uid = auth.principal.user_id;
    let pattern = db::get_pattern_pool(pool, &id).await?;
    if pattern.uid.as_deref() != Some(&uid) {
        return Err(CommandError::Unauthorized(
            "You can only verify your own patterns".to_string(),
        ));
    }

    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let display_name = remote_queries::fetch_user_profile(&client, &uid, &auth.access_token)
        .await
        .map_err(|error| CommandError::Internal(format!("Failed to fetch profile: {error}")))?
        .unwrap_or_else(|| uid.clone());

    // Both writes bump updated_at, marking the row dirty for the push below.
    db::set_author_name(pool, &id, &display_name).await?;
    db::set_verified(pool, &id, verify).await?;

    services
        .sync
        .run_push(&uid)
        .await
        .map_err(|error| CommandError::Internal(format!("Failed to sync pattern: {error}")))?;

    Ok(db::get_pattern_pool(pool, &id).await?)
}

/// Resolves against a venue, unlike `get_pattern_graph_document`, which passes
/// `venue_id = None` — so the two can return *different* implementations for
/// the same pattern id. See `pattern-args-venue-divergence` in the IPC manifest.
pub async fn get_pattern_args(
    services: &AppServices,
    id: String,
    venue_id: Option<String>,
    implementation_id: Option<String>,
) -> Result<Vec<PatternArgDef>, CommandError> {
    let pool = &services.db.0;
    db::get_pattern_pool(pool, &id).await?;
    let document =
        load_visible_graph_document(pool, &id, venue_id.as_deref(), implementation_id.as_deref())
            .await
            .map_err(|error| CommandError::Internal(error.to_string()))?;
    Ok(document.graph.args)
}

/// Idempotent by `operation_id` + base revision. The TS caller reuses the id
/// verbatim on retry; that reuse is the only thing making a blind retry safe.
pub async fn save_pattern_graph_document(
    services: &AppServices,
    id: String,
    implementation_id: String,
    operation_id: String,
    base_revision: String,
    graph: Graph,
) -> Result<GraphEditResult, CommandError> {
    let owner_user_id = services.session_user_id().await?;
    let result = services
        .authored
        .apply_graph_for_scope(
            &services.db.0,
            owner_user_id.as_deref(),
            &id,
            &implementation_id,
            &operation_id,
            graph,
            &base_revision,
            "Save pattern graph",
        )
        .await
        .map_err(|error| CommandError::Internal(error.to_string()))?;
    let AuthoredProjectedDocument::PatternGraph {
        implementation_id: projected_implementation_id,
        revision,
        graph,
    } = result.document
    else {
        return Err(CommandError::Internal(
            "authored graph save returned a track projection".into(),
        ));
    };
    if projected_implementation_id != implementation_id {
        return Err(CommandError::Internal(
            "authored graph save returned another implementation".into(),
        ));
    }
    if result.changed {
        services.sync.push_notify.notify_one();
    }
    Ok(GraphEditResult {
        revision,
        graph,
        changed: result.changed,
    })
}
