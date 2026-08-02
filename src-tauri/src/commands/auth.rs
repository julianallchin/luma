//! Tauri commands for auth session storage

use sqlx::{Sqlite, SqliteConnection, Transaction};
use tauri::{AppHandle, State};

use crate::database::local::state::StateDb;
use crate::database::Db;
use crate::services::authored_documents::AuthoredDocuments;
use crate::sync::orchestrator::{enqueue_dirty, SyncEngine};
use crate::sync::{push, registry};

#[tauri::command]
pub async fn get_session_item(
    key: String,
    state: State<'_, StateDb>,
    db: State<'_, Db>,
    engine: State<'_, SyncEngine>,
) -> Result<Option<String>, String> {
    if key != crate::database::local::auth::SUPABASE_SESSION_KEY {
        return crate::database::local::auth::get_session_item(&state.0, &key).await;
    }
    let _sync = engine.sync_lock.lock().await;
    let mut session_guard = state
        .0
        .acquire()
        .await
        .map_err(|error| format!("Failed to lock authenticated session: {error}"))?;
    if crate::database::local::auth::recover_committed_signout(&db.0, &mut session_guard).await? {
        return Ok(
            crate::database::local::auth::load_renderer_session_for_connection(&mut session_guard)
                .await?
                .map(|(session, _)| session),
        );
    }
    drop(session_guard);

    // Bootstrap legacy state or refresh a proven expiring token while the
    // identity-transition lock excludes account switches and sync work.
    crate::database::local::auth::get_session_item(&state.0, &key).await?;
    let mut session_guard = state
        .0
        .acquire()
        .await
        .map_err(|error| format!("Failed to lock authenticated session: {error}"))?;
    let recovered =
        crate::database::local::auth::recover_committed_signout(&db.0, &mut session_guard).await?;
    let renderer =
        crate::database::local::auth::load_renderer_session_for_connection(&mut session_guard)
            .await?;
    if !recovered {
        crate::database::local::auth::arm_write_admission(
            &db.0,
            renderer
                .as_ref()
                .and_then(|(_, principal)| principal.as_ref())
                .map(|principal| principal.user_id.as_str()),
        )
        .await?;
    }
    Ok(renderer.map(|(session, _)| session))
}

#[tauri::command]
pub async fn set_session_item(
    key: String,
    value: String,
    state: State<'_, StateDb>,
    db: State<'_, Db>,
    engine: State<'_, SyncEngine>,
    authored: State<'_, AuthoredDocuments>,
    workspaces: State<'_, crate::agent_execution::PythonWorkspaceService>,
    graph_runs: State<'_, crate::agent_execution::graph_runs::GraphRunStore>,
    host_audio: State<'_, crate::host_audio::HostAudioState>,
    render_engine: State<'_, crate::render_engine::RenderEngine>,
    controller: State<'_, crate::controller_manager::ControllerManager>,
    mixer: State<'_, crate::mixer_manager::MixerManager>,
    stem_cache: State<'_, crate::audio::StemCache>,
    analysis_tasks: State<'_, crate::preprocessing::AnalysisTaskGroup>,
) -> Result<(), String> {
    if key == crate::database::local::auth::SUPABASE_SESSION_KEY {
        let validated = crate::database::local::auth::validate_supabase_session(&value).await?;
        let principal = validated.principal();
        let _sync = engine.sync_lock.lock().await;
        let mut session_guard = state
            .0
            .acquire()
            .await
            .map_err(|error| format!("Failed to lock authenticated session: {error}"))?;
        crate::database::local::auth::recover_committed_signout(&db.0, &mut session_guard).await?;
        let replacement = crate::database::local::auth::session_replacement_kind_for_connection(
            &mut session_guard,
            &principal,
        )
        .await?;
        let admission_backup =
            crate::database::local::auth::capture_write_admission(&db.0, &mut session_guard)
                .await?;

        // Supabase routinely emits TOKEN_REFRESHED for the same user. The
        // sync lock and reserved StateDb connection serialize that credential
        // rotation; app-database authority and every live capability remain
        // the same, so resetting Python/audio/render state would be wrong.
        if replacement == crate::database::local::auth::SessionReplacementKind::CredentialRefresh {
            crate::database::local::auth::replace_session_for_connection(
                &mut session_guard,
                &validated,
            )
            .await?;
            if let Err(error) = crate::commands::agent_threads::recover_deleting_agent_threads(
                &db.0,
                &authored,
                &workspaces,
                &graph_runs,
            )
            .await
            {
                eprintln!("[agent-threads] identity-activation deletion recovery: {error}");
            }
            return Ok(());
        }

        let backup =
            crate::database::local::auth::capture_auth_state_for_connection(&mut session_guard)
                .await?;
        let _authored_barrier = authored.begin_identity_switch().await;
        // Closing app-database admission is the first cross-identity commit
        // fence. Its write lock waits out any already-admitted Operate guard
        // and prevents a later host-audio/render/device effect from racing in
        // after the process-global caches are cleared.
        crate::database::local::auth::suspend_write_admission(&db.0, &admission_backup).await?;
        let _analysis_barrier = match analysis_tasks.suspend_for_identity_switch().await {
            Ok(barrier) => barrier,
            Err(error) => {
                return rollback_auth_switch(
                    &db.0,
                    &mut session_guard,
                    &backup,
                    &admission_backup,
                    error,
                )
                .await;
            }
        };
        let _workspace_barrier = workspaces.suspend_for_identity_switch().await;
        graph_runs.clear();
        host_audio.unload();
        render_engine.reset_for_identity_switch();
        if let Err(error) = controller.disconnect() {
            return rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await;
        }
        if let Err(error) = mixer.disconnect() {
            return rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await;
        }
        stem_cache.clear();
        if let Err(error) = crate::database::local::auth::replace_session_for_connection(
            &mut session_guard,
            &validated,
        )
        .await
        {
            return rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await;
        }
        if let Err(error) =
            crate::database::local::auth::arm_write_admission(&db.0, Some(&principal.user_id)).await
        {
            return rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await;
        }
        if let Err(error) = crate::commands::agent_threads::recover_deleting_agent_threads(
            &db.0,
            &authored,
            &workspaces,
            &graph_runs,
        )
        .await
        {
            // Session installation has already committed. Cleanup remains in
            // its durable terminal state and will retry on refresh/startup;
            // never report the identity switch itself as rolled back.
            eprintln!("[agent-threads] identity-activation deletion recovery: {error}");
        }
        Ok(())
    } else {
        crate::database::local::auth::set_session_item(&state.0, &key, &value).await
    }
}

#[tauri::command]
pub async fn remove_session_item(
    key: String,
    state: State<'_, StateDb>,
    db: State<'_, Db>,
    engine: State<'_, SyncEngine>,
    authored: State<'_, AuthoredDocuments>,
) -> Result<(), String> {
    if key == crate::database::local::auth::SUPABASE_SESSION_KEY {
        let _sync = engine.sync_lock.lock().await;
        let mut session_guard = state
            .0
            .acquire()
            .await
            .map_err(|error| format!("Failed to lock authenticated session: {error}"))?;
        crate::database::local::auth::recover_committed_signout(&db.0, &mut session_guard).await?;
        let backup =
            crate::database::local::auth::capture_auth_state_for_connection(&mut session_guard)
                .await?;
        let admission_backup =
            crate::database::local::auth::capture_write_admission(&db.0, &mut session_guard)
                .await?;
        let _authored_barrier = authored.begin_identity_switch().await;
        crate::database::local::auth::suspend_write_admission(&db.0, &admission_backup).await?;
        if let Err(error) = crate::database::local::auth::consume_signout_transition_and_clear_session_for_connection(
            &mut session_guard,
        )
        .await
        {
            return rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await;
        }
        if let Err(error) = crate::database::local::auth::arm_write_admission(&db.0, None).await {
            return rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await;
        }
        return Ok(());
    }
    crate::database::local::auth::remove_session_item(&state.0, &key).await
}

async fn rollback_auth_switch(
    pool: &sqlx::SqlitePool,
    connection: &mut SqliteConnection,
    backup: &crate::database::local::auth::AuthStateBackup,
    admission_backup: &crate::database::local::auth::WriteAdmissionSnapshot,
    cause: String,
) -> Result<(), String> {
    if let Err(rollback_error) =
        crate::database::local::auth::restore_auth_state_for_connection(connection, backup).await
    {
        return Err(format!(
            "Authenticated identity switch failed: {cause}. Restoring the previous session also failed; signed writes remain disabled: {rollback_error}"
        ));
    }
    if let Err(rollback_error) =
        crate::database::local::auth::restore_write_admission(pool, admission_backup).await
    {
        return Err(format!(
            "Authenticated identity switch failed: {cause}. The previous session was restored, but restoring its write admission failed; signed writes remain disabled: {rollback_error}"
        ));
    }
    Err(format!(
        "Authenticated identity switch failed and the previous session was restored: {cause}"
    ))
}

#[tauri::command]
pub async fn log_session_from_state_db(state: State<'_, StateDb>) -> Result<(), String> {
    crate::database::local::auth::log_supabase_session(&state.0).await
}

/// Sign out's host-side commit boundary. The authenticated session remains
/// installed while all cloud catalog state is made durable, every authored
/// graph/score is reconciled to Git, and only the signed-in relational
/// projection is removed. Any failure aborts before deleting catalog state.
#[tauri::command]
pub async fn wipe_database(
    app_handle: AppHandle,
    db: State<'_, Db>,
    state: State<'_, StateDb>,
    authored: State<'_, AuthoredDocuments>,
    engine: State<'_, SyncEngine>,
    workspaces: State<'_, crate::agent_execution::PythonWorkspaceService>,
    graph_runs: State<'_, crate::agent_execution::graph_runs::GraphRunStore>,
    host_audio: State<'_, crate::host_audio::HostAudioState>,
    render_engine: State<'_, crate::render_engine::RenderEngine>,
    controller: State<'_, crate::controller_manager::ControllerManager>,
    mixer: State<'_, crate::mixer_manager::MixerManager>,
    stem_cache: State<'_, crate::audio::StemCache>,
    analysis_tasks: State<'_, crate::preprocessing::AnalysisTaskGroup>,
) -> Result<(), String> {
    // Exclude the background pull/push loop for the whole boundary. It must
    // not repopulate relational rows while logout is proving and removing the
    // current projection.
    let _sync = engine.sync_lock.lock().await;
    {
        let mut session_guard = state
            .0
            .acquire()
            .await
            .map_err(|error| format!("Failed to lock authenticated session: {error}"))?;
        if crate::database::local::auth::recover_committed_signout(&db.0, &mut session_guard)
            .await?
        {
            return Ok(());
        }
    }
    let (_, principal) = engine
        .require_auth()
        .await
        .map_err(|error| format!("Cannot sign out without an authenticated session: {error}"))?;

    // Tracks may need their file/storage metadata finalized before their
    // catalog rows are safe to remove locally. Offline or partial sync is a
    // logout failure, never permission to discard the only catalog copy.
    engine
        .sync_files_unlocked(&app_handle)
        .await
        .map_err(|error| format!("Cannot sign out before files are durable: {error}"))?;
    enqueue_dirty(&db.0, &principal)
        .await
        .map_err(|error| format!("Cannot sign out before catalog sync: {error}"))?;
    push::flush_pending(&db.0, &state.0, engine.remote().as_ref())
        .await
        .map_err(|error| format!("Cannot sign out before catalog sync: {error}"))?;

    // StateDb has one connection by construction. Keeping it checked out
    // freezes session persistence until the wipe commits. Re-reading after all
    // network work catches a concurrent refresh/sign-in/sign-out race.
    let mut session_guard = state
        .0
        .acquire()
        .await
        .map_err(|error| format!("Failed to lock authenticated session: {error}"))?;
    let frozen_principal =
        crate::database::local::auth::load_verified_principal_for_connection(&mut session_guard)
            .await?;
    if frozen_principal
        .as_ref()
        .map(|value| value.user_id.as_str())
        != Some(principal.as_str())
    {
        return Err(
            "Authenticated session changed while preparing sign-out; nothing was deleted".into(),
        );
    }

    // Analysis owns SQLite and cache publication, so close its admission and
    // drain the current generation before taking the wipe transaction's write
    // lock. No cache is cleared yet; the database fence below still precedes
    // every process-global capability reset.
    let _analysis_barrier = analysis_tasks.suspend_for_identity_switch().await?;

    // Drain authored mutations and keep their global write guard through the
    // wipe. Every graph/score that committed before this point is now in Git;
    // later authored mutations cannot begin.
    let _prepared = authored
        .prepare_sign_out(&db.0)
        .await
        .map_err(|error| format!("Refusing database wipe: {error}"))?;

    let mut transaction =
        db.0.begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| format!("Failed to begin database wipe: {error}"))?;
    close_signed_write_admission(&mut transaction, &principal).await?;

    // BEGIN IMMEDIATE first waits out every admitted live operation. Keeping
    // that write fence through the reset prevents new host-audio/render/device
    // effects from completing after prior-principal capabilities are cleared.
    // The Python barrier additionally drains cells holding immutable manifests
    // or a TrackHost before relational visibility changes.
    let _workspace_barrier = workspaces.suspend_for_identity_switch().await;
    graph_runs.clear();
    host_audio.unload();
    render_engine.reset_for_identity_switch();
    controller.disconnect()?;
    mixer.disconnect()?;
    stem_cache.clear();

    assert_signed_in_catalog_durable(&mut transaction, &principal).await?;
    wipe_signed_in_projection(&mut transaction, &principal).await?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit database wipe: {error}"))?;
    let recovered =
        crate::database::local::auth::recover_committed_signout(&db.0, &mut session_guard)
            .await
            .map_err(|error| {
                format!(
                    "Signed-in projection was made durable and removed, but sign-out recovery failed; writes remain disabled until recovery completes: {error}"
                )
            })?;
    if !recovered {
        return Err(
            "Signed-in projection was made durable and removed, but its committed sign-out journal was not recoverable; writes remain disabled"
                .into(),
        );
    }
    println!("[auth] Signed-in database projection wiped on sign-out");
    Ok(())
}

async fn close_signed_write_admission(
    transaction: &mut Transaction<'_, Sqlite>,
    principal: &str,
) -> Result<(), String> {
    let result = sqlx::query(
        "UPDATE auth_write_admission
         SET accepting = 0, maintenance = 1, generation = generation + 1
         WHERE singleton = 1 AND armed = 1 AND accepting = 1 AND active_uid = ?",
    )
    .bind(principal)
    .execute(&mut **transaction)
    .await
    .map_err(|error| format!("Failed to close signed-write admission: {error}"))?;
    if result.rows_affected() != 1 {
        return Err(
            "Signed-write admission no longer belongs to the authenticated principal; nothing was deleted"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) async fn wipe_database_pool(
    pool: &sqlx::SqlitePool,
    authored: &AuthoredDocuments,
    principal: &str,
) -> Result<(), String> {
    // The guard proves that every relational graph and score has been imported
    // into durable Git and prevents a concurrent authored mutation from
    // appearing before the wipe commits. Reconciliation failure aborts before
    // the destructive transaction begins.
    let _prepared = authored
        .prepare_sign_out(pool)
        .await
        .map_err(|error| format!("Refusing database wipe: {error}"))?;
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin database wipe: {error}"))?;
    close_signed_write_admission(&mut transaction, principal).await?;
    assert_signed_in_catalog_durable(&mut transaction, principal).await?;
    wipe_signed_in_projection(&mut transaction, principal).await?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit database wipe: {error}"))?;
    Ok(())
}

async fn assert_signed_in_catalog_durable(
    transaction: &mut Transaction<'_, Sqlite>,
    principal: &str,
) -> Result<(), String> {
    audit_uid_bearing_tables(&mut **transaction).await?;
    let pending_principal = crate::database::local::auth::principal_key(Some(principal));
    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pending_ops WHERE principal_key = ?")
            .bind(&pending_principal)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| format!("Failed to inspect pending catalog sync: {error}"))?;
    if pending != 0 {
        return Err(format!(
            "Refusing database wipe: {pending} catalog operation(s) are still pending remote sync"
        ));
    }

    for table in registry::TABLES {
        if !table.columns.contains(&"uid") {
            return Err(format!(
                "Refusing database wipe: sync table {} has no principal column",
                table.name
            ));
        }
        let count_sql = format!(
            "SELECT COUNT(*) FROM {} WHERE uid = ? AND synced_at IS NULL",
            table.name
        );
        let dirty: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(count_sql))
            .bind(principal)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| {
                format!(
                    "Failed to inspect {} catalog durability: {error}",
                    table.name
                )
            })?;
        if dirty != 0 {
            return Err(format!(
                "Refusing database wipe: {dirty} signed-in {} row(s) are not durably synced",
                table.name
            ));
        }
    }
    let dirty_categories: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pattern_categories
         WHERE uid = ? AND synced_at IS NULL",
    )
    .bind(principal)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| format!("Failed to inspect pattern category durability: {error}"))?;
    if dirty_categories != 0 {
        return Err(format!(
            "Refusing database wipe: {dirty_categories} signed-in pattern category row(s) are not durably synced"
        ));
    }
    Ok(())
}

async fn audit_uid_bearing_tables(connection: &mut SqliteConnection) -> Result<(), String> {
    let actual: Vec<String> = sqlx::query_scalar(
        "SELECT schema.name
         FROM sqlite_schema schema
         JOIN pragma_table_info(schema.name) column ON column.name = 'uid'
         WHERE schema.type = 'table'
         ORDER BY schema.name",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| format!("Failed to audit principal-bearing tables: {error}"))?;
    let classified = [
        "cues",
        "fixture_group_members",
        "fixture_groups",
        "fixtures",
        "implementations",
        "midi_bindings",
        "midi_modifiers",
        "pattern_categories",
        "patterns",
        "scores",
        "stage_pieces",
        "sync_state",
        "track_bar_classifications",
        "track_beats",
        "track_drum_onsets",
        "track_roots",
        "track_scores",
        "track_stems",
        "track_waveforms",
        "tracks",
        "venue_implementation_overrides",
        "venues",
    ];
    let unknown: Vec<_> = actual
        .iter()
        .filter(|table| !classified.contains(&table.as_str()))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "Refusing database wipe: principal-bearing table(s) lack an explicit durability policy: {}",
            unknown
                .into_iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(())
}

async fn wipe_signed_in_projection(
    transaction: &mut Transaction<'_, Sqlite>,
    principal: &str,
) -> Result<(), String> {
    let principal_key = crate::database::local::auth::principal_key(Some(principal));

    // Git repositories and ledgers always survive logout. A projection that
    // remains part of a retained physical venue closure stays present; only
    // non-retained documents owned by this exact principal become absent.
    sqlx::query(
        "UPDATE authored_state_projections
         SET materialization_state = 'absent'
         WHERE principal_key = ?
           AND materialization_state = 'present'
           AND NOT (
               (document_kind = 'track_score' AND EXISTS(
                   SELECT 1 FROM scores score
                   WHERE score.id = authored_state_projections.score_id
               ))
               OR
               (document_kind = 'pattern_graph' AND EXISTS(
                   SELECT 1 FROM patterns pattern
                   WHERE pattern.id = authored_state_projections.subject_id
                     AND (
                         EXISTS(SELECT 1 FROM track_scores clip
                                WHERE clip.pattern_id = pattern.id)
                         OR EXISTS(SELECT 1 FROM cues cue
                                   WHERE cue.pattern_id = pattern.id)
                         OR EXISTS(SELECT 1 FROM venue_implementation_overrides override
                                   WHERE override.pattern_id = pattern.id)
                     )
               ))
           )",
    )
    .bind(&principal_key)
    .execute(&mut **transaction)
    .await
    .map_err(|error| format!("Failed to mark signed-in authored projections absent: {error}"))?;

    // Venue rows are a sealed local cache, not an ephemeral session
    // projection. Retain the whole physical aggregate and every relational
    // dependency needed to render its scores losslessly. Remove only this
    // principal's unreferenced catalog leaves; another cached principal and
    // guest state are never touched.
    for statement in [
        "DELETE FROM patterns
         WHERE uid = ?
           AND NOT EXISTS(SELECT 1 FROM track_scores clip
                          WHERE clip.pattern_id = patterns.id)
           AND NOT EXISTS(SELECT 1 FROM cues cue
                          WHERE cue.pattern_id = patterns.id)
           AND NOT EXISTS(SELECT 1 FROM venue_implementation_overrides override
                          WHERE override.pattern_id = patterns.id)",
        "DELETE FROM pattern_categories WHERE uid = ?",
        "DELETE FROM tracks
         WHERE uid = ?
           AND NOT EXISTS(SELECT 1 FROM scores score
                          WHERE score.track_id = tracks.id)",
    ] {
        sqlx::query(statement)
            .bind(principal)
            .execute(&mut **transaction)
            .await
            .map_err(|error| format!("Failed to remove signed-in catalog projection: {error}"))?;
    }
    sqlx::query("DELETE FROM sync_state WHERE uid = ?")
        .bind(principal)
        .execute(&mut **transaction)
        .await
        .map_err(|error| format!("Failed to reset signed-in pull cursors: {error}"))?;
    sqlx::query(
        "UPDATE auth_write_admission SET maintenance = 0
         WHERE singleton = 1 AND maintenance = 1 AND accepting = 0",
    )
    .execute(&mut **transaction)
    .await
    .map_err(|error| format!("Failed to leave logout maintenance mode: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageRoot;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    #[tokio::test]
    async fn sign_out_wipe_preserves_routing_and_marks_projections_absent() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("luma-test.db");
        let migrate_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations")
            .run(&migrate_pool)
            .await
            .unwrap();
        migrate_pool.close().await;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', 'alice', 'p')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES ('implementation', 'alice', 'pattern', '{\"nodes\":[],\"edges\":[],\"args\":[]}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_threads
             (id, owner_user_id, agent_kind, subject_kind, subject_id, implementation_id)
             VALUES ('thread', 'alice', 'pattern_graph', 'pattern', 'pattern', 'implementation')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_thread_messages (id, thread_id, seq, role, parts_json)
             VALUES ('message', 'thread', 0, 'user', '[{\"type\":\"text\",\"text\":\"keep me\"}]')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let authored =
            AuthoredDocuments::new(StorageRoot::from_path(directory.path().join("storage")));
        authored
            .reconcile_available_projections(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE patterns
             SET synced_at = updated_at, version = version + 1
             WHERE uid = 'alice'",
        )
        .execute(&pool)
        .await
        .unwrap();

        wipe_database_pool(&pool, &authored, "alice").await.unwrap();

        let retained: (String, String) = sqlx::query_as(
            "SELECT subject_id, implementation_id FROM agent_threads WHERE id = 'thread'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(retained, ("pattern".into(), "implementation".into()));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM implementations")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM patterns")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT materialization_state FROM authored_state_projections
                 WHERE subject_id = 'pattern' AND implementation_id = 'implementation'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "absent"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT parts_json FROM agent_thread_messages WHERE id = 'message'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "[{\"type\":\"text\",\"text\":\"keep me\"}]"
        );
    }

    #[tokio::test]
    async fn sign_out_wipe_fails_before_deleting_unimportable_authored_state() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("luma-test.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('pattern', 'alice', 'p')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO implementations (id, uid, pattern_id, graph_json)
             VALUES ('implementation', 'alice', 'pattern', 'not-json')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let authored =
            AuthoredDocuments::new(StorageRoot::from_path(directory.path().join("storage")));

        let error = wipe_database_pool(&pool, &authored, "alice")
            .await
            .unwrap_err();
        assert!(error.contains("Refusing database wipe"), "{error}");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM patterns")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM implementations")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn admission_and_explicit_dirtiness_are_database_invariants() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("admission.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(database)
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();

        sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('alice-pattern', 'alice', 'p')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE patterns SET synced_at = updated_at, version = version + 1
             WHERE id = 'alice-pattern'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE patterns SET name = 'edited' WHERE id = 'alice-pattern'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT synced_at FROM patterns WHERE id = 'alice-pattern'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            None
        );

        let forged_remote = sqlx::query(
            "INSERT INTO patterns (id, uid, name, origin)
             VALUES ('bob-pattern', 'bob', 'forged', 'remote')",
        )
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(forged_remote
            .to_string()
            .contains("signed-in write admission is closed or principal-mismatched"));
        assert!(
            sqlx::query("UPDATE patterns SET uid = NULL WHERE id = 'alice-pattern'")
                .execute(&pool)
                .await
                .is_err()
        );

        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .unwrap();
        assert!(
            sqlx::query("UPDATE patterns SET name = 'stale' WHERE id = 'alice-pattern'")
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("DELETE FROM patterns WHERE id = 'alice-pattern'")
                .execute(&pool)
                .await
                .is_err()
        );
        sqlx::query("INSERT INTO patterns (id, name) VALUES ('guest-pattern', 'guest')")
            .execute(&pool)
            .await
            .unwrap();
    }
}
