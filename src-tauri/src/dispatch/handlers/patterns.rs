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
            .await?;
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
        .await?;
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use serde_json::{json, Value};

    use crate::database::local::{auth, database, state};
    use crate::dispatch::{dispatch, AppServices, CommandError};

    /// Fixed idempotency keys, so a replay of the fixture is a replay rather
    /// than a second pattern. The authored store validates them as UUIDs.
    const REQUEST_ID: &str = "6b2f0c10-0000-4000-8000-000000000001";
    const FIRST_WRITE: &str = "6b2f0c10-0000-4000-8000-000000000002";
    const SECOND_WRITE: &str = "6b2f0c10-0000-4000-8000-000000000003";

    /// A lost optimistic-concurrency check has to reach the host *as* a
    /// conflict: an editor recovers from a stale base by re-reading, and
    /// cannot tell that from a storage failure if both arrive as `Internal`.
    #[tokio::test]
    async fn a_stale_base_revision_is_refused_as_a_conflict_carrying_both_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let services = seed(directory.path()).await;

        let pattern = dispatch(
            &services,
            "create_pattern",
            &json!({ "requestId": REQUEST_ID, "name": "Race", "description": null }),
        )
        .await
        .expect("the fixture pattern was not created");
        let id = pattern["id"].as_str().unwrap().to_string();

        let document = graph_document(&services, &id).await;
        let implementation = document["implementationId"].as_str().unwrap().to_string();
        let stale = document["revision"].as_str().unwrap().to_string();

        // The winner moves the head; the loser is still holding what the head
        // was before it.
        let won = save(
            &services,
            &id,
            &implementation,
            FIRST_WRITE,
            &stale,
            node("n0"),
        )
        .await
        .expect("the first write was refused");
        let current = won["revision"].as_str().unwrap().to_string();
        assert_ne!(current, stale, "the first write did not move the head");

        let error = save(
            &services,
            &id,
            &implementation,
            SECOND_WRITE,
            &stale,
            node("n1"),
        )
        .await
        .expect_err("a write against a stale base was accepted");
        let CommandError::Conflict {
            expected, found, ..
        } = &error
        else {
            panic!("a lost race arrived as {} ({error})", error.kind());
        };
        assert_eq!(expected.as_deref(), Some(stale.as_str()));
        assert_eq!(found.as_deref(), Some(current.as_str()));
    }

    /// A one-node graph. Distinct ids give distinct content, and the revision
    /// is content-addressed — two writes of the same graph would leave the
    /// head where it was and there would be no race to lose.
    fn node(id: &str) -> Value {
        json!({
            "nodes": [{ "id": id, "typeId": "ramp", "params": {},
                        "positionX": 0.0, "positionY": 0.0 }],
            "edges": [],
            "args": [],
        })
    }

    async fn save(
        services: &AppServices,
        id: &str,
        implementation_id: &str,
        operation_id: &str,
        base_revision: &str,
        graph: Value,
    ) -> Result<Value, CommandError> {
        dispatch(
            services,
            "save_pattern_graph_document",
            &json!({
                "id": id,
                "implementationId": implementation_id,
                "operationId": operation_id,
                "baseRevision": base_revision,
                "graph": graph,
            }),
        )
        .await
    }

    async fn graph_document(services: &AppServices, id: &str) -> Value {
        dispatch(
            services,
            "get_pattern_graph_document",
            &json!({ "id": id, "implementationId": null }),
        )
        .await
        .expect("the pattern has no graph document")
    }

    async fn seed(directory: &Path) -> AppServices {
        let db = database::init_app_db_at(directory).await.unwrap();
        let state_db = state::init_state_db_at(directory).await.unwrap();
        auth::bootstrap_host_admission(&db.0, &state_db.0)
            .await
            .unwrap();
        let storage = crate::storage::StorageRoot::from_path(directory.to_path_buf());
        let workspaces = Arc::new(
            crate::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("no Python here".to_string())),
            ),
        );
        AppServices::headless(db, state_db, storage, directory.to_path_buf(), workspaces)
    }
}
