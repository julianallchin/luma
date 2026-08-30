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
#[cfg(test)]
use crate::services::authored_documents::AuthoredDocuments;
use crate::sync::orchestrator::enqueue_dirty;
use crate::sync::{push, registry};

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
    let engine = &services.sync;
    if key != crate::database::local::auth::SUPABASE_SESSION_KEY {
        return Ok(crate::database::local::auth::get_session_item(&state.0, &key).await?);
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
        let principal = renderer
            .as_ref()
            .and_then(|(_, principal)| principal.as_ref())
            .map(|principal| principal.user_id.as_str());
        let authored_barrier = engine.authored().begin_identity_switch().await;
        let activated =
            crate::database::local::auth::arm_write_admission_for_identity_switch(&db.0, principal)
                .await?;
        if let Err(error) = engine
            .authored()
            .bootstrap_live_projections_during_identity_switch(&db.0, principal, &authored_barrier)
            .await
        {
            let close = crate::database::local::auth::suspend_write_admission_for_rollback(
                &db.0, &activated,
            )
            .await;
            return Err(CommandError::Internal(match close {
                Ok(_) => format!(
                    "Failed to bootstrap authored projections; signed writes remain closed: {error}"
                ),
                Err(close_error) => format!(
                    "Failed to bootstrap authored projections, and closing the activated admission failed: {error}; {close_error}"
                ),
            }));
        }
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
    let engine = &services.sync;
    let authored = &services.authored;
    let workspaces = &services.workspaces;
    let graph_runs = &services.graph_runs;
    let subagents = &services.subagents;
    let host_audio = &services.host_audio;
    let render_engine = &services.render_engine;
    let controller = &services.controller;
    let mixer = &services.mixer;
    let stem_cache = &services.stem_cache;
    let analysis_tasks = &services.analysis_tasks;
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
            let authored_barrier = authored.begin_identity_switch().await;
            crate::database::local::auth::replace_session_for_connection(
                &mut session_guard,
                &validated,
            )
            .await?;
            if let Err(error) = authored
                .bootstrap_live_projections_during_identity_switch(
                    &db.0,
                    Some(&principal.user_id),
                    &authored_barrier,
                )
                .await
            {
                let close = crate::database::local::auth::suspend_write_admission_for_rollback(
                    &db.0,
                    &admission_backup,
                )
                .await;
                return Err(CommandError::Internal(match close {
                    Ok(_) => format!(
                        "Failed to bootstrap authored projections after credential refresh; signed writes remain closed: {error}"
                    ),
                    Err(close_error) => format!(
                        "Failed to bootstrap authored projections after credential refresh, and closing admission failed: {error}; {close_error}"
                    ),
                }));
            }
            if let Err(error) = crate::agent_execution::thread_cleanup::recover_threads(
                &db.0, authored, workspaces, graph_runs, subagents,
            )
            .await
            {
                eprintln!("[agent-threads] identity-activation recovery: {error}");
            }
            return Ok(());
        }

        let backup =
            crate::database::local::auth::capture_auth_state_for_connection(&mut session_guard)
                .await?;
        let _authored_barrier = authored.begin_identity_switch().await;
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
        let activated_admission =
            match crate::database::local::auth::arm_write_admission_for_identity_switch(
                &db.0,
                Some(&principal.user_id),
            )
            .await
            {
                Ok(admission) => admission,
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
        if let Err(error) = authored
            .bootstrap_live_projections_during_identity_switch(
                &db.0,
                Some(&principal.user_id),
                &_authored_barrier,
            )
            .await
        {
            return Err(rollback_activated_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                &activated_admission,
                format!("authored projection bootstrap failed: {error}"),
            )
            .await);
        }
        if let Err(error) = crate::agent_execution::thread_cleanup::recover_threads(
            &db.0, authored, workspaces, graph_runs, subagents,
        )
        .await
        {
            // Session installation has already committed. Cleanup remains in
            // its durable terminal state and will retry on refresh/startup;
            // never report the identity switch itself as rolled back.
            eprintln!("[agent-threads] identity-activation recovery: {error}");
        }
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
    let engine = &services.sync;
    let authored = &services.authored;
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
            return Err(rollback_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                error,
            )
            .await);
        }
        let activated_admission =
            match crate::database::local::auth::arm_write_admission_for_identity_switch(&db.0, None)
                .await
            {
                Ok(admission) => admission,
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
        if let Err(error) = authored
            .bootstrap_live_projections_during_identity_switch(&db.0, None, &_authored_barrier)
            .await
        {
            return Err(rollback_activated_auth_switch(
                &db.0,
                &mut session_guard,
                &backup,
                &admission_backup,
                &activated_admission,
                format!("authored projection bootstrap failed: {error}"),
            )
            .await);
        }
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

/// Undo an identity switch that failed *after* the replacement was admitted.
/// Same contract as [`rollback_auth_switch`], one extra fence.
async fn rollback_activated_auth_switch(
    pool: &sqlx::SqlitePool,
    connection: &mut SqliteConnection,
    backup: &crate::database::local::auth::AuthStateBackup,
    previous_admission: &crate::database::local::auth::WriteAdmissionSnapshot,
    activated_admission: &crate::database::local::auth::WriteAdmissionSnapshot,
    cause: String,
) -> CommandError {
    // Bootstrap failed after the replacement identity was admitted. Close
    // that exact generation first; StateDb and the prior principal may only
    // be restored while all durable writes remain fenced out.
    let closed = match crate::database::local::auth::suspend_write_admission_for_rollback(
        pool,
        activated_admission,
    )
    .await
    {
        Ok(closed) => closed,
        Err(close_error) => {
            return CommandError::Internal(format!(
                "Authenticated identity switch failed: {cause}. Closing the newly admitted identity also failed; refusing stale rollback: {close_error}"
            ));
        }
    };
    if let Err(rollback_error) =
        crate::database::local::auth::restore_auth_state_for_connection(connection, backup).await
    {
        return CommandError::Internal(format!(
            "Authenticated identity switch failed: {cause}. Signed writes were closed, but restoring the previous session failed: {rollback_error}"
        ));
    }
    if let Err(rollback_error) = crate::database::local::auth::restore_write_admission_from_closed(
        pool,
        previous_admission,
        &closed,
    )
    .await
    {
        return CommandError::Internal(format!(
            "Authenticated identity switch failed: {cause}. The previous session was restored, but its write admission remains closed: {rollback_error}"
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
    let authored = &services.authored;
    let engine = &services.sync;
    let workspaces = &services.workspaces;
    let graph_runs = &services.graph_runs;
    let host_audio = &services.host_audio;
    let render_engine = &services.render_engine;
    let controller = &services.controller;
    let mixer = &services.mixer;
    let stem_cache = &services.stem_cache;
    let analysis_tasks = &services.analysis_tasks;

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
        .sync_files_unlocked(&services.sync_host())
        .await
        .map_err(|error| format!("Cannot sign out before files are durable: {error}"))?;
    enqueue_dirty(&db.0, &principal)
        .await
        .map_err(|error| format!("Cannot sign out before catalog sync: {error}"))?;
    push::flush_pending_with_integrator(
        &db.0,
        &state.0,
        engine.remote().as_ref(),
        Some(engine.authored()),
    )
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
    // wipe. The empty principal-scoped pending queue above proves every prior
    // revision/trace is remote; later authored mutations cannot begin.
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
    // The lifecycle barrier prevents a concurrent authored mutation from
    // appearing after the durability audit and before the wipe commits.
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
    audit_uid_bearing_tables(transaction).await?;
    let pending_principal = crate::database::local::auth::principal_key(Some(principal));
    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pending_ops WHERE principal_key = ?")
            .bind(&pending_principal)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| format!("Failed to inspect pending catalog sync: {error}"))?;
    if pending != 0 {
        return Err(format!(
            "Refusing database wipe: {pending} operation(s) are still pending remote sync"
        ));
    }

    for table in registry::TABLES {
        // Explicit immutable/authority operations are covered by the empty
        // principal-scoped pending queue above. Only dirty-row tables expose
        // the uid/synced_at delivery contract checked here.
        if registry::push_policy(table.name) != registry::PushPolicy::DirtyUpsert {
            continue;
        }
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

/// Every principal-bearing table, each one *read by a human* and given a
/// durability policy above.
///
/// This is deliberately a hand-maintained list and deliberately not derived
/// from the schema. `venue_owned_tables` answers a different question — "which
/// rows belong to this venue", for `sync::pull`'s delete guard — and deriving
/// this one from it turned the audit into a tautology: a new table with a `uid`
/// and a `venue_id` classified itself, which is exactly the review the audit
/// exists to force. The cost of the list is one line per new table, paid by
/// whoever knows what the table is for.
const CLASSIFIED: &[&str] = &[
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
    "track_genres",
    "track_roots",
    "track_scores",
    "track_stems",
    "track_waveforms",
    "tracks",
    "venue_implementation_overrides",
    // The venue graph. Synced since
    // `supabase/migrations/20260902000000_venue_graph_sync_shape.sql`, and not
    // wiped either — see `wipe_signed_in_projection`: venue content is a sealed
    // local cache that survives sign-out, so a row that has not been pushed yet
    // is still there to push next time.
    "venue_constraints",
    "venue_edges",
    "venue_node_params",
    "venue_nodes",
    "venues",
];

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
    let unknown: Vec<_> = actual
        .iter()
        .filter(|table| !CLASSIFIED.contains(&table.as_str()))
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
    // Venue rows are a sealed local cache, not an ephemeral session
    // projection. Relational revision history and its live projection also
    // survive logout: removing either would require a second materialization
    // ledger. Remove only catalog leaves that own no authored document;
    // another cached principal and guest state are never touched.
    for statement in [
        "DELETE FROM patterns
         WHERE uid = ?
           AND NOT EXISTS(
               SELECT 1 FROM authored_documents document
               WHERE document.document_kind = 'pattern_graph'
                 AND document.subject_id = patterns.id
           )
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
    use std::path::Path;

    use super::*;
    use crate::storage::StorageRoot;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use sqlx::SqlitePool;

    #[tokio::test]
    async fn sign_out_wipe_preserves_relational_authored_history_and_projection() {
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
        let authored =
            AuthoredDocuments::new(StorageRoot::from_path(directory.path().join("storage")));
        authored
            .create_thread_with_authored_state(
                &pool,
                crate::models::agent_threads::CreateAgentThreadInput {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    agent_kind: "pattern_graph".into(),
                    subject_kind: Some("pattern".into()),
                    subject_id: Some("pattern".into()),
                    implementation_id: Some("implementation".into()),
                    ..Default::default()
                },
                Some("alice"),
            )
            .await
            .unwrap();
        // Model a completed remote flush. Append-only authored rows carry
        // delivery state exclusively in the principal-scoped pending queue.
        sqlx::query("DELETE FROM pending_ops WHERE principal_key = 'signed-in:alice'")
            .execute(&pool)
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

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM implementations")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM patterns")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM authored_revisions")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    /// A pool on a freshly migrated schema, with `alice` admitted to write.
    async fn migrated_pool(directory: &Path) -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.join("audit.db"))
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
        pool
    }

    /// The audit's whole job: a table nobody classified stops the wipe.
    ///
    /// It is a hand-maintained list on purpose. Derived from the schema — from
    /// `venue_owned_tables`, say — a new table would answer for itself and this
    /// test would pass with nobody having looked at it.
    #[tokio::test]
    async fn an_unclassified_principal_bearing_table_fails_the_audit() {
        let directory = tempfile::tempdir().unwrap();
        let pool = migrated_pool(directory.path()).await;
        let mut connection = pool.acquire().await.unwrap();
        audit_uid_bearing_tables(&mut connection)
            .await
            .expect("the shipped schema is fully classified");

        sqlx::query(
            "CREATE TABLE venue_annotations (
                 id TEXT PRIMARY KEY, uid TEXT, venue_id TEXT NOT NULL, note TEXT)",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        let error = audit_uid_bearing_tables(&mut connection)
            .await
            .expect_err("a new principal-bearing table has no durability policy yet");
        assert!(
            error.contains("venue_annotations"),
            "the audit has to name what it does not know about: {error}"
        );
    }

    /// An unsynced venue graph blocks sign-out, and sign-out keeps it.
    ///
    /// Two separate promises, and the graph needs both. The audit refuses the
    /// wipe while any registered row of alice's is still dirty — the graph is
    /// registered now, so it counts exactly as an unpushed fixture does. And
    /// once it is clean, the wipe still leaves it alone:
    /// `wipe_signed_in_projection` treats venue content as a sealed local
    /// cache, so signing out never destroys a rig.
    #[tokio::test]
    async fn an_unsynced_venue_graph_blocks_sign_out_and_survives_it() {
        let directory = tempfile::tempdir().unwrap();
        let pool = migrated_pool(directory.path()).await;
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('ven', 'alice', 'Basement')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO venue_nodes (id, uid, venue_id, kind, label)
             VALUES ('ven:venue', 'alice', 'ven', 'venue', 'Room')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // The venue row itself is in the sync registry and is durable; only the
        // graph hanging off it is not, which is the case no sweep can see.
        sqlx::query("UPDATE venues SET synced_at = updated_at, version = version + 1")
            .execute(&pool)
            .await
            .unwrap();
        let authored =
            AuthoredDocuments::new(StorageRoot::from_path(directory.path().join("storage")));
        let error = wipe_database_pool(&pool, &authored, "alice")
            .await
            .expect_err("a dirty graph row is undelivered work");
        assert!(error.contains("venue_nodes"), "{error}");

        sqlx::query("UPDATE venue_nodes SET synced_at = updated_at, version = version + 1")
            .execute(&pool)
            .await
            .unwrap();
        wipe_database_pool(&pool, &authored, "alice").await.unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM venue_nodes")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1,
            "signing out must not destroy a venue graph"
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
