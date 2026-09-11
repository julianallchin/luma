//! Sign-in, auth session storage, and the identity transitions that hang off
//! them.
//!
//! Sign-in is email OTP and nothing else. Both halves of it live here rather
//! than in a renderer because the host already owns the Supabase endpoint and
//! key, and because verifying a code has to hand its session straight to the
//! identity-switch boundary below without a round trip through a client.
//!
//! Three of the storage commands are session *storage* in name only: writing,
//! reading or clearing the Supabase session key is Luma's identity-switch
//! boundary, and it has to fence every process-global capability (Python
//! workspaces, host audio, render, MIDI devices, stem cache, analysis) against
//! the previous principal before the new one is admitted.
//!
//! Their error strings are contractual — they distinguish "the previous session
//! was restored" from "signed writes remain closed" — so the rollback helpers
//! at the bottom own that prose and no caller re-wraps it.

use sqlx::{Sqlite, SqliteConnection, Transaction};

use crate::dispatch::{AppServices, CommandError};

/// Ask Supabase to email a six-digit login code.
///
/// Email OTP is Luma's only sign-in method: no password, no OAuth, no redirect
/// URL. A renderer that owned this call would have to own `SUPABASE_URL` and
/// the anon key too, and the host already does — so both halves of the
/// exchange live here and a renderer only says which email and which code.
///
/// # Errors
///
/// If Supabase is unreachable, or rejects the address.
pub async fn send_login_code(_services: &AppServices, email: String) -> Result<(), CommandError> {
    let email = email.trim();
    if email.is_empty() {
        return Err(CommandError::Internal("Enter an email address".into()));
    }
    otp_post(
        "otp",
        &serde_json::json!({ "email": email, "create_user": true }),
    )
    .await?;
    Ok(())
}

/// Exchange an emailed code for a session, and install it.
///
/// Installation goes through [`set_session_item`] rather than a second
/// persistence path: verifying a code is how a session is *obtained*, but
/// admitting one is an identity switch, and there is exactly one place that
/// fences the process-global capabilities that switch invalidates.
///
/// Returns the principal now admitted, so a caller holding a cached identity
/// does not have to re-derive it from the database it just wrote.
///
/// # Errors
///
/// If Supabase is unreachable, the code is wrong or expired, or the identity
/// switch fails — in which case [`set_session_item`]'s rollback prose says
/// what survived.
pub async fn verify_login_code(
    services: &AppServices,
    email: String,
    code: String,
) -> Result<String, CommandError> {
    let session = otp_post(
        "verify",
        &serde_json::json!({
            "email": email.trim(),
            "token": code.trim(),
            "type": "email",
        }),
    )
    .await?;
    set_session_item(
        services,
        crate::database::local::auth::SUPABASE_SESSION_KEY.to_string(),
        session,
    )
    .await?;
    crate::database::local::auth::get_current_user_id(&services.state_db.0)
        .await?
        .ok_or_else(|| {
            CommandError::Internal("Supabase accepted the code but installed no session".into())
        })
}

/// Who this library belongs to: the admitted principal and the address to name
/// it by, or `None` for the guest namespace.
///
/// Offline by construction — see
/// [`crate::database::local::auth::load_current_account`]. A host reads this at
/// launch, so it must never be a reason a launch fails.
///
/// # Errors
///
/// If the stored session and its host proof disagree.
pub async fn current_account(
    services: &AppServices,
) -> Result<Option<crate::database::local::auth::AuthAccount>, CommandError> {
    Ok(crate::database::local::auth::load_current_account(&services.state_db.0).await?)
}

/// One POST to a GoTrue endpoint that answers with JSON, returned verbatim.
///
/// Verbatim matters: `/verify`'s body *is* the session blob the host persists,
/// and re-serializing it through a typed struct would drop whatever fields
/// Supabase added since — the very bytes the principal proof is a hash of.
async fn otp_post(endpoint: &str, body: &serde_json::Value) -> Result<String, CommandError> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| format!("Failed to initialize auth client: {error}"))?;
    let response = client
        .post(format!(
            "{}/auth/v1/{endpoint}",
            crate::config::SUPABASE_URL.trim_end_matches('/')
        ))
        .header("apikey", crate::config::SUPABASE_ANON_KEY)
        .json(body)
        .send()
        .await
        .map_err(|error| format!("Could not reach Supabase: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Supabase returned an unreadable response: {error}"))?;
    if !status.is_success() {
        return Err(CommandError::Internal(supabase_message(status, &text)));
    }
    Ok(text)
}

/// GoTrue's own words where it has any. Its errors are the ones a person
/// acting on them needs — "Token has expired or is invalid" is actionable
/// where "auth request failed (403)" is not — so the status is only the
/// fallback for a body that carries no message.
fn supabase_message(status: reqwest::StatusCode, body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            for key in ["error_description", "msg", "message", "error"] {
                if let Some(text) = value.get(key).and_then(serde_json::Value::as_str) {
                    if !text.trim().is_empty() {
                        return Some(text.trim().to_string());
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| format!("Supabase rejected the request ({status})"))
}

/// Read one session item. For the Supabase session key this is a *getter with
/// write side effects*: Supabase's storage adapter calls it on client
/// construction, so legacy-state bootstrap, token refresh and write-admission
/// arming all hang off it.
pub async fn get_session_item(
    services: &AppServices,
    key: String,
) -> Result<Option<String>, CommandError> {
    let state = &services.state_db;
    let db = &services.db;
    if key != crate::database::local::auth::SUPABASE_SESSION_KEY {
        return Ok(crate::database::local::auth::get_session_item(&state.0, &key).await?);
    }
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
        let principal = renderer
            .as_ref()
            .and_then(|(_, principal)| principal.as_ref())
            .map(|principal| principal.user_id.as_str());
        crate::database::local::auth::arm_write_admission_for_identity_switch(&db.0, principal)
            .await?;
    }
    Ok(renderer.map(|(session, _)| session))
}

/// Write one session item. For the Supabase session key this classifies the
/// write as a credential refresh (same principal — keep every live capability)
/// or an identity switch (different principal — fence and reset all of them),
/// with staged rollback on either path.
pub async fn set_session_item(
    services: &AppServices,
    key: String,
    value: String,
) -> Result<(), CommandError> {
    let state = &services.state_db;
    let db = &services.db;
    let workspaces = &services.workspaces;
    let graph_runs = &services.graph_runs;
    let host_audio = &services.host_audio;
    let render_engine = &services.render_engine;
    let controller = &services.controller;
    let mixer = &services.mixer;
    let stem_cache = &services.stem_cache;
    let analysis_tasks = &services.analysis_tasks;
    if key == crate::database::local::auth::SUPABASE_SESSION_KEY {
        let validated = crate::database::local::auth::validate_supabase_session(&value).await?;
        let principal = validated.principal();
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
            return Ok(());
        }

        let backup =
            crate::database::local::auth::capture_auth_state_for_connection(&mut session_guard)
                .await?;
        // Imports may have published a phase-one track row and still own a
        // cancellation rollback. Drain them while the old principal remains
        // admitted; closing admission first would make the compensating
        // deletion fail and strand both the row and managed audio.
        let _analysis_barrier = match analysis_tasks.suspend_for_identity_switch().await {
            Ok(barrier) => barrier,
            Err(error) => {
                return Err(rollback_auth_switch(
                    &db.0,
                    &mut session_guard,
                    &backup,
                    &admission_backup,
                    error,
                )
                .await);
            }
        };
        // Closing app-database admission is the first cross-identity commit
        // fence after import compensation. Its write lock waits out every
        // other admitted operation and prevents a later host effect from
        // racing in after process-global caches are cleared.
        crate::database::local::auth::suspend_write_admission(&db.0, &admission_backup).await?;
        let _workspace_barrier = workspaces.suspend_for_identity_switch().await;
        graph_runs.clear();
        host_audio.unload();
        render_engine.reset_for_identity_switch();
        if let Err(error) = controller.disconnect() {
            return Err(rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await);
        }
        if let Err(error) = mixer.disconnect() {
            return Err(rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await);
        }
        stem_cache.clear();
        if let Err(error) = crate::database::local::auth::replace_session_for_connection(
            &mut session_guard,
            &validated,
        )
        .await
        {
            return Err(rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await);
        }
        match crate::database::local::auth::arm_write_admission_for_identity_switch(
                &db.0,
                Some(&principal.user_id),
            )
            .await
            {
                Ok(_) => {}
                Err(error) => {
                    return Err(rollback_auth_switch(
                        &db.0,
                        &mut session_guard,
                        &backup,
                        &admission_backup,
                        error,
                    )
                    .await);
                }
            };
        Ok(())
    } else {
        Ok(crate::database::local::auth::set_session_item(&state.0, &key, &value).await?)
    }
}

/// Clear one session item. Clearing the Supabase session key is the *sign-out*
/// transition, not a cache delete.
///
/// SMELL: unlike [`set_session_item`]'s identity-switch branch, this path does
/// not reset host audio / render / devices / Python. It relies on
/// [`wipe_database`] having run first — the frontend store calls that, then
/// `signOut()`, which drives this. The asymmetry is load-bearing only by
/// convention.
pub async fn remove_session_item(services: &AppServices, key: String) -> Result<(), CommandError> {
    let state = &services.state_db;
    let db = &services.db;
    if key == crate::database::local::auth::SUPABASE_SESSION_KEY {
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
        crate::database::local::auth::suspend_write_admission(&db.0, &admission_backup).await?;
        if let Err(error) = crate::database::local::auth::consume_signout_transition_and_clear_session_for_connection(
            &mut session_guard,
        )
        .await
        {
            return Err(rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await);
        }
        match crate::database::local::auth::arm_write_admission_for_identity_switch(&db.0, None)
                .await
            {
                Ok(_) => {}
                Err(error) => {
                    return Err(rollback_auth_switch(
                        &db.0,
                        &mut session_guard,
                        &backup,
                        &admission_backup,
                        error,
                    )
                    .await);
                }
            };
        return Ok(());
    }
    Ok(crate::database::local::auth::remove_session_item(&state.0, &key).await?)
}

/// Undo an identity switch that failed before the replacement was admitted.
///
/// Returns the error rather than a `Result`: every path here is a failure, and
/// the value is which failure the caller reports. The three messages are a
/// contract — "restored", "signed writes remain disabled" and "refusing stale
/// rollback" mean different things to a user staring at a sign-in dialog.
async fn rollback_auth_switch(
    pool: &sqlx::SqlitePool,
    connection: &mut SqliteConnection,
    backup: &crate::database::local::auth::AuthStateBackup,
    admission_backup: &crate::database::local::auth::WriteAdmissionSnapshot,
    cause: String,
) -> CommandError {
    if let Err(rollback_error) =
        crate::database::local::auth::restore_auth_state_for_connection(connection, backup).await
    {
        return CommandError::Internal(format!(
            "Authenticated identity switch failed: {cause}. Restoring the previous session also failed; signed writes remain disabled: {rollback_error}"
        ));
    }
    if let Err(rollback_error) =
        crate::database::local::auth::restore_write_admission(pool, admission_backup).await
    {
        return CommandError::Internal(format!(
            "Authenticated identity switch failed: {cause}. The previous session was restored, but restoring its write admission failed; signed writes remain disabled: {rollback_error}"
        ));
    }
    CommandError::Internal(format!(
        "Authenticated identity switch failed and the previous session was restored: {cause}"
    ))
}


/// Sign out's host-side commit boundary. The authenticated session remains
/// installed while all cloud catalog state, authored revision history, and
/// conversation traces are made durable. Any failure aborts before deleting
/// signed-in catalog state.
pub async fn wipe_database(services: &AppServices) -> Result<(), CommandError> {
    let db = &services.db;
    let state = &services.state_db;
    let workspaces = &services.workspaces;
    let graph_runs = &services.graph_runs;
    let host_audio = &services.host_audio;
    let render_engine = &services.render_engine;
    let controller = &services.controller;
    let mixer = &services.mixer;
    let stem_cache = &services.stem_cache;
    let analysis_tasks = &services.analysis_tasks;

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

    // StateDb has one connection by construction. Keeping it checked out
    // freezes session persistence until the wipe commits, which is what
    // catches a concurrent refresh/sign-in/sign-out race.
    let mut session_guard = state
        .0
        .acquire()
        .await
        .map_err(|error| format!("Failed to lock authenticated session: {error}"))?;
    let principal =
        crate::database::local::auth::load_verified_principal_for_connection(&mut session_guard)
            .await?
            .map(|principal| principal.user_id)
            .ok_or("Cannot sign out without an authenticated session")?;

    // Analysis owns SQLite and cache publication, so close its admission and
    // drain the current generation before taking the wipe transaction's write
    // lock.
    let _analysis_barrier = analysis_tasks.suspend_for_identity_switch().await?;

    let mut transaction =
        db.0.begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| format!("Failed to begin database wipe: {error}"))?;
    close_signed_write_admission(&mut transaction, &principal).await?;

    // BEGIN IMMEDIATE first waits out every admitted live operation. Keeping
    // that write fence through the reset prevents new host-audio/render/device
    // effects from completing after prior-principal capabilities are cleared.
    let _workspace_barrier = workspaces.suspend_for_identity_switch().await;
    graph_runs.clear();
    host_audio.unload();
    render_engine.reset_for_identity_switch();
    controller.disconnect()?;
    mixer.disconnect()?;
    stem_cache.clear();

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
                    "Signed-in projection was removed, but sign-out recovery failed; writes remain disabled until recovery completes: {error}"
                )
            })?;
    if !recovered {
        return Err(
            "Signed-in projection was removed, but its committed sign-out journal was not recoverable; writes remain disabled"
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
    principal: &str,
) -> Result<(), String> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin database wipe: {error}"))?;
    close_signed_write_admission(&mut transaction, principal).await?;
    wipe_signed_in_projection(&mut transaction, principal).await?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit database wipe: {error}"))?;
    Ok(())
}

async fn wipe_signed_in_projection(
    transaction: &mut Transaction<'_, Sqlite>,
    principal: &str,
) -> Result<(), String> {
    // Venue rows are a sealed local cache, not an ephemeral session
    // projection, and they survive logout. Remove only catalog leaves nothing
    // else depends on; another cached principal and guest state are never
    // touched.
    for statement in [
        "DELETE FROM patterns
         WHERE uid = ?
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
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    /// Signing out removes the catalog leaves nothing else depends on, and
    /// leaves everything that is still referenced — a pattern a cue plays, a
    /// track a score annotates — exactly where it is.
    #[tokio::test]
    async fn sign_out_keeps_what_the_library_still_refers_to() {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("wipe.db"))
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
        for statement in [
            "INSERT INTO venues (id, uid, name) VALUES ('ven', 'alice', 'Basement')",
            "INSERT INTO tracks (id, uid, track_hash, file_path) VALUES ('t', 'alice', 'h', '/t')",
            "INSERT INTO scores (id, uid, track_id, venue_id) VALUES ('s', 'alice', 't', 'ven')",
            "INSERT INTO patterns (id, uid, name) VALUES ('kept', 'alice', 'Kept')",
            "INSERT INTO patterns (id, uid, name) VALUES ('loose', 'alice', 'Loose')",
            "INSERT INTO cues (id, uid, venue_id, name, pattern_id)
             VALUES ('c', 'alice', 'ven', 'Cue', 'kept')",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }

        wipe_database_pool(&pool, "alice").await.unwrap();

        let remaining: Vec<String> =
            sqlx::query_scalar("SELECT id FROM patterns ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, ["kept"]);
        // A track a score still annotates is not a leaf.
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracks")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM venues")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1,
            "signing out must not destroy a venue"
        );
    }

    #[tokio::test]
    async fn admission_is_a_database_invariant() {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("admission.db"))
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
        let forged = sqlx::query("INSERT INTO patterns (id, uid, name) VALUES ('bob', 'bob', 'f')")
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(forged
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
