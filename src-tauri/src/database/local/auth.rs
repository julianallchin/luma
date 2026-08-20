use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Acquire, SqliteConnection, SqlitePool};

use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};

pub const SUPABASE_SESSION_KEY: &str = "supabase_session";
const PROOF_SINGLETON: i64 = 1;
const AUTHENTICATED_AUDIENCE: &str = "authenticated";
const MAX_SESSION_JSON_BYTES: usize = 1_048_576;
const MAX_ACCESS_TOKEN_BYTES: usize = 65_536;
const REFRESH_WINDOW_SECONDS: i64 = 60;

/// The only identity callers may use for authorization or routing. It is
/// reconstructed from a host-only proof bound to the exact persisted token,
/// never from renderer-owned `session.user` JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedPrincipal {
    pub user_id: String,
    pub expires_at: i64,
}

/// An exact, host-verified session snapshot. Keeping the token and principal
/// together prevents a session switch between two independent reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAuth {
    pub principal: VerifiedPrincipal,
    pub access_token: String,
}

/// Opaque result of online validation. Only this type can be persisted with a
/// host proof, so commands cannot accidentally arm admission from raw JSON.
pub(crate) struct ValidatedSession {
    session_json: String,
    envelope: SessionEnvelope,
    proof: PrincipalProof,
}

impl ValidatedSession {
    pub(crate) fn principal(&self) -> VerifiedPrincipal {
        self.proof.principal()
    }
}

#[derive(Clone, Debug, Deserialize)]
struct SessionEnvelope {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    user: Option<SessionUserHint>,
}

#[derive(Clone, Debug, Deserialize)]
struct SessionUserHint {
    id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(untagged)]
enum JwtAudience {
    One(String),
    Many(Vec<String>),
}

impl JwtAudience {
    fn contains(&self, expected: &str) -> bool {
        match self {
            Self::One(value) => value == expected,
            Self::Many(values) => values.iter().any(|value| value == expected),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct JwtClaims {
    sub: String,
    iss: String,
    aud: JwtAudience,
    exp: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrincipalProof {
    user_id: String,
    session_sha256: String,
    access_token_sha256: String,
    jwt_sub: String,
    jwt_issuer: String,
    jwt_audience_json: String,
    jwt_expires_at: i64,
    verified_at: i64,
    proof_generation: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SignoutTransition {
    user_id: String,
    session_sha256: String,
    proof_generation: String,
    issued_at: i64,
}

/// Exact StateDb bytes captured before a cross-database identity switch. The
/// command layer may restore this only after admission has been closed; it can
/// never derive a principal from the backup without the normal proof checks.
pub(crate) struct AuthStateBackup {
    session_json: Option<String>,
    proof: Option<PrincipalProof>,
    transition: Option<SignoutTransition>,
}

/// Whether installing a host-validated session merely rotates credentials for
/// the already-admitted principal or crosses a runtime capability boundary.
/// Legacy sessions intentionally count as an identity transition: without a
/// host proof their app-database authority is the guest namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionReplacementKind {
    CredentialRefresh,
    IdentityTransition,
}

pub(crate) const SIGNED_OUT_PRINCIPAL_KEY: &str = "signed-out";

/// Canonical durable namespace shared by authored revision metadata and the sync
/// queue. It is deliberately distinct from nullable SQL ownership so keys are
/// stable in logs, hashes, and cross-table associations.
pub(crate) fn principal_key(principal: Option<&str>) -> String {
    principal.map_or_else(
        || SIGNED_OUT_PRINCIPAL_KEY.to_owned(),
        |id| format!("signed-in:{id}"),
    )
}

/// Host-only snapshot of the app database's authenticated-write gate. Auth
/// commands capture this while holding the global sync lock, close admission,
/// and may restore the exact prior mode only if the gate is still in the
/// closed state produced by that command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WriteAdmissionSnapshot {
    accepting: bool,
    active_uid: Option<String>,
    generation: i64,
}

/// Exact closed generation produced while rolling back an identity that had
/// already been armed. It prevents restoring the previous principal across a
/// later lifecycle transition.
#[derive(Debug)]
pub(crate) struct ClosedWriteAdmission {
    generation: i64,
}

impl PrincipalProof {
    fn principal(&self) -> VerifiedPrincipal {
        VerifiedPrincipal {
            user_id: self.user_id.clone(),
            expires_at: self.jwt_expires_at,
        }
    }
}

#[derive(Clone, Debug)]
struct VerifiedSnapshot {
    session_json: String,
    envelope: SessionEnvelope,
    proof: PrincipalProof,
}

#[async_trait]
trait AuthServer: Send + Sync {
    async fn authenticated_user_id(&self, access_token: &str) -> Result<String, String>;
    async fn refresh(&self, refresh_token: &str) -> Result<String, String>;
}

struct SupabaseAuthServer {
    client: reqwest::Client,
}

impl SupabaseAuthServer {
    fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| format!("Failed to initialize auth client: {error}"))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl AuthServer for SupabaseAuthServer {
    async fn authenticated_user_id(&self, access_token: &str) -> Result<String, String> {
        let response = self
            .client
            .get(format!(
                "{}/auth/v1/user",
                SUPABASE_URL.trim_end_matches('/')
            ))
            .header("apikey", SUPABASE_ANON_KEY)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| format!("Supabase session validation failed: {error}"))?;

        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(format!(
                "Supabase rejected the access token ({status}): {}",
                bounded_response_detail(&detail)
            ));
        }

        #[derive(Deserialize)]
        struct AuthenticatedUser {
            id: String,
        }

        let user: AuthenticatedUser = response
            .json()
            .await
            .map_err(|error| format!("Supabase returned an invalid user response: {error}"))?;
        if user.id.trim().is_empty() {
            return Err("Supabase returned an empty authenticated user id".into());
        }
        Ok(user.id)
    }

    async fn refresh(&self, refresh_token: &str) -> Result<String, String> {
        let response = self
            .client
            .post(format!(
                "{}/auth/v1/token?grant_type=refresh_token",
                SUPABASE_URL.trim_end_matches('/')
            ))
            .header("apikey", SUPABASE_ANON_KEY)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "refresh_token": refresh_token }))
            .send()
            .await
            .map_err(|error| format!("Supabase session refresh failed: {error}"))?;

        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(format!(
                "Supabase rejected the refresh token ({status}): {}",
                bounded_response_detail(&detail)
            ));
        }

        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|error| format!("Supabase returned an invalid refresh response: {error}"))?;
        serde_json::to_string(&value)
            .map_err(|error| format!("Failed to serialize refreshed session: {error}"))
    }
}

pub async fn initialize_auth_state_schema(pool: &SqlitePool) -> Result<(), String> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| format!("Failed to initialize auth state: {error}"))?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS auth_session (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to initialize auth session table: {error}"))?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS auth_principal_proof (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            user_id TEXT NOT NULL,
            session_sha256 TEXT NOT NULL,
            access_token_sha256 TEXT NOT NULL,
            jwt_sub TEXT NOT NULL,
            jwt_issuer TEXT NOT NULL,
            jwt_audience_json TEXT NOT NULL,
            jwt_expires_at INTEGER NOT NULL,
            verified_at INTEGER NOT NULL,
            proof_generation TEXT NOT NULL
        )",
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to initialize authenticated-principal proof: {error}"))?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS auth_signout_transition (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            user_id TEXT NOT NULL,
            session_sha256 TEXT NOT NULL,
            proof_generation TEXT NOT NULL,
            issued_at INTEGER NOT NULL
        )",
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to initialize sign-out transition state: {error}"))?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit auth state initialization: {error}"))
}

/// Open the app-database admission invariant for a host-verified principal or
/// for the guest namespace. Closed lifecycle states use
/// [`suspend_write_admission`] or the committed-wipe journal; `None` is not a
/// synonym for closed.
pub async fn arm_write_admission(pool: &SqlitePool, principal: Option<&str>) -> Result<(), String> {
    arm_write_admission_for_identity_switch(pool, principal)
        .await
        .map(drop)
}

/// Arm and return the exact new gate generation. Identity-switch callers keep
/// this capability until every fallible bootstrap step succeeds; on failure
/// they can CAS-close the newly admitted identity before restoring StateDb.
pub(crate) async fn arm_write_admission_for_identity_switch(
    pool: &SqlitePool,
    principal: Option<&str>,
) -> Result<WriteAdmissionSnapshot, String> {
    if principal.is_some_and(str::is_empty) {
        return Err("Signed-write admission principal cannot be empty".into());
    }
    let row = sqlx::query_as::<_, (i64, Option<String>, i64)>(
        "UPDATE auth_write_admission
         SET armed = 1, accepting = ?, maintenance = 0, remote_writes = 0,
             active_uid = ?, generation = generation + 1
         WHERE singleton = 1 AND generation < 9223372036854775807
         RETURNING accepting, active_uid, generation",
    )
    .bind(1_i64)
    .bind(principal)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Failed to update signed-write admission: {error}"))?;
    let (accepting, active_uid, generation) = row.ok_or_else(|| {
        "Signed-write admission singleton is missing or its generation overflowed".to_string()
    })?;
    if accepting != 1 || active_uid.as_deref() != principal || generation < 0 {
        return Err("Signed-write admission returned an invalid armed capability".into());
    }
    Ok(WriteAdmissionSnapshot {
        accepting: true,
        active_uid,
        generation,
    })
}

/// Return the principal currently admitted by the app database. Credentials
/// in `StateDb` establish who may be admitted, but this row is the sole live
/// authority used by app-data operations. `None` is the active guest
/// namespace; a closed/suspended admission is an error, not a guest fallback.
pub(crate) async fn admitted_principal(pool: &SqlitePool) -> Result<Option<String>, String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT active_uid FROM auth_write_admission
         WHERE singleton = 1 AND armed = 1 AND accepting = 1
           AND maintenance = 0 AND remote_writes = 0",
    )
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Failed to read active app principal: {error}"))?
    .ok_or_else(|| "App database admission is closed".to_owned())
}

/// Capture a quiescent admission mode before an identity transition. The sync
/// lock is the caller's capability: maintenance and remote-write modes can
/// only be entered by other holders of that lock and are therefore rejected.
pub(crate) async fn capture_write_admission(
    pool: &SqlitePool,
    state_connection: &mut SqliteConnection,
) -> Result<WriteAdmissionSnapshot, String> {
    let row = sqlx::query_as::<_, (i64, i64, i64, i64, Option<String>, i64)>(
        "SELECT armed, accepting, maintenance, remote_writes, active_uid, generation
         FROM auth_write_admission WHERE singleton = 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Failed to capture signed-write admission: {error}"))?
    .ok_or_else(|| "Signed-write admission singleton is missing".to_string())?;
    let (armed, accepting, maintenance, remote_writes, active_uid, generation) = row;
    let armed = admission_bit(armed, "armed")?;
    let accepting = admission_bit(accepting, "accepting")?;
    let maintenance = admission_bit(maintenance, "maintenance")?;
    let remote_writes = admission_bit(remote_writes, "remote_writes")?;
    if !armed {
        return Err("Signed-write admission was not initialized by the host".into());
    }
    if maintenance || remote_writes {
        return Err("Signed-write admission is not quiescent during an identity transition".into());
    }
    if active_uid.as_deref().is_some_and(str::is_empty) {
        return Err("Signed-write admission has an empty active principal".into());
    }
    if generation < 0 {
        return Err("Signed-write admission has an invalid generation".into());
    }
    let snapshot = WriteAdmissionSnapshot {
        accepting,
        active_uid,
        generation,
    };
    validate_admission_matches_auth_state(state_connection, &snapshot).await?;
    Ok(snapshot)
}

/// Close all ordinary venue and signed writes for one identity transition.
/// The exact captured row is the capability, and the compare-and-swap makes
/// suspension fail rather than closing a newer principal's admission.
pub(crate) async fn suspend_write_admission(
    pool: &SqlitePool,
    snapshot: &WriteAdmissionSnapshot,
) -> Result<(), String> {
    suspend_write_admission_for_rollback(pool, snapshot)
        .await
        .map(drop)
}

pub(crate) async fn suspend_write_admission_for_rollback(
    pool: &SqlitePool,
    snapshot: &WriteAdmissionSnapshot,
) -> Result<ClosedWriteAdmission, String> {
    let closed_generation = snapshot
        .generation
        .checked_add(1)
        .ok_or_else(|| "Signed-write admission generation overflow".to_string())?;
    let result = sqlx::query(
        "UPDATE auth_write_admission
         SET accepting = 0, maintenance = 0, remote_writes = 0,
             active_uid = NULL, generation = generation + 1
         WHERE singleton = 1 AND armed = 1 AND accepting = ?
           AND maintenance = 0 AND remote_writes = 0 AND active_uid IS ?
           AND generation = ?",
    )
    .bind(i64::from(snapshot.accepting))
    .bind(snapshot.active_uid.as_deref())
    .bind(snapshot.generation)
    .execute(pool)
    .await
    .map_err(|error| format!("Failed to suspend signed-write admission: {error}"))?;
    if result.rows_affected() != 1 {
        return Err(
            "Signed-write admission changed before it could be suspended; retry the transition"
                .into(),
        );
    }
    Ok(ClosedWriteAdmission {
        generation: closed_generation,
    })
}

async fn validate_admission_matches_auth_state(
    connection: &mut SqliteConnection,
    admission: &WriteAdmissionSnapshot,
) -> Result<(), String> {
    let session_json =
        read_raw_session_item_for_connection(connection, SUPABASE_SESSION_KEY).await?;
    let proof = read_proof_for_connection(connection).await?;
    let transition = read_signout_transition_for_connection(connection).await?;
    match (session_json, proof, transition) {
        (None, None, None) => require_admission_mode(admission, true, None),
        (None, _, _) => Err("Authenticated auth state has no matching session".into()),
        (Some(session_json), None, None) => {
            // A legacy session carries no host authority. It may be upgraded
            // online, but until then the app database must remain guest-only.
            let envelope = parse_session(&session_json)?;
            let claims = parse_and_validate_claims(&envelope.access_token, unix_now(), true)?;
            validate_user_hint(&envelope, &claims.sub)?;
            require_admission_mode(admission, true, None)
        }
        (Some(_), None, Some(_)) => {
            Err("Sign-out transition has no matching authenticated proof".into())
        }
        (Some(session_json), Some(proof), transition) => {
            validate_snapshot_parts(&session_json, &proof, unix_now(), true)?;
            if let Some(transition) = transition {
                validate_transition(&transition, &proof)?;
                require_admission_mode(admission, false, Some(&proof.user_id))
            } else {
                require_admission_mode(admission, true, Some(&proof.user_id))
            }
        }
    }
}

fn require_admission_mode(
    admission: &WriteAdmissionSnapshot,
    accepting: bool,
    active_uid: Option<&str>,
) -> Result<(), String> {
    if admission.accepting == accepting && admission.active_uid.as_deref() == active_uid {
        Ok(())
    } else {
        Err("App-database admission does not match the authenticated host state".into())
    }
}

/// Restore a previously captured admission mode after StateDb rollback. The
/// compare-and-swap proves no other lifecycle transition changed the gate
/// after this command closed it. Generation stays monotonic rather than being
/// rewound to the captured value.
pub(crate) async fn restore_write_admission(
    pool: &SqlitePool,
    snapshot: &WriteAdmissionSnapshot,
) -> Result<(), String> {
    let closed_generation = snapshot
        .generation
        .checked_add(1)
        .ok_or_else(|| "Signed-write admission generation overflow".to_string())?;
    restore_write_admission_from_closed(
        pool,
        snapshot,
        &ClosedWriteAdmission {
            generation: closed_generation,
        },
    )
    .await
}

pub(crate) async fn restore_write_admission_from_closed(
    pool: &SqlitePool,
    snapshot: &WriteAdmissionSnapshot,
    closed: &ClosedWriteAdmission,
) -> Result<(), String> {
    let result = sqlx::query(
        "UPDATE auth_write_admission
         SET armed = 1, accepting = ?, maintenance = 0, remote_writes = 0,
             active_uid = ?, generation = generation + 1
         WHERE singleton = 1 AND armed = 1 AND accepting = 0
           AND maintenance = 0 AND remote_writes = 0 AND active_uid IS NULL
           AND generation = ?",
    )
    .bind(i64::from(snapshot.accepting))
    .bind(snapshot.active_uid.as_deref())
    .bind(closed.generation)
    .execute(pool)
    .await
    .map_err(|error| format!("Failed to restore signed-write admission: {error}"))?;
    if result.rows_affected() != 1 {
        return Err(
            "Signed-write admission changed after it was closed; refusing stale rollback".into(),
        );
    }
    Ok(())
}

fn admission_bit(value: i64, field: &str) -> Result<bool, String> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(format!(
            "Signed-write admission has invalid {field} value {value}"
        )),
    }
}

/// Renderer-readable storage. The Supabase session key is special: an old
/// session without a proof is returned only after online server validation has
/// bootstrapped that proof. Other values carry no authorization authority.
pub async fn get_session_item(pool: &SqlitePool, key: &str) -> Result<Option<String>, String> {
    if key == SUPABASE_SESSION_KEY {
        return load_or_bootstrap_verified_session(pool).await;
    }
    get_raw_session_item(pool, key).await
}

pub async fn set_session_item(pool: &SqlitePool, key: &str, value: &str) -> Result<(), String> {
    if key == SUPABASE_SESSION_KEY {
        return Err("Supabase sessions require host validation before persistence".into());
    }
    set_raw_session_item(pool, key, value).await
}

pub async fn remove_session_item(pool: &SqlitePool, key: &str) -> Result<(), String> {
    if key == SUPABASE_SESSION_KEY {
        return Err("Supabase session removal requires the host sign-out boundary".into());
    }
    sqlx::query("DELETE FROM auth_session WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await
        .map_err(|error| format!("Failed to remove session value: {error}"))?;
    Ok(())
}

/// Validate a candidate with Supabase and bind the authentic server user to
/// the exact JWT claims and bytes that will be persisted.
pub(crate) async fn validate_supabase_session(
    session_json: &str,
) -> Result<ValidatedSession, String> {
    let server = SupabaseAuthServer::new()?;
    validate_session_with(session_json, &server, unix_now()).await
}

/// Atomically install session bytes and their proof through a connection held
/// by the caller. The auth command keeps this one StateDb connection reserved
/// across app-db admission changes, serializing concurrent identity switches.
pub(crate) async fn replace_session_for_connection(
    connection: &mut SqliteConnection,
    validated: &ValidatedSession,
) -> Result<(), String> {
    session_replacement_kind_for_connection(connection, &validated.principal()).await?;
    persist_validated_session_unchecked(connection, validated).await
}

async fn persist_validated_session_unchecked(
    connection: &mut SqliteConnection,
    validated: &ValidatedSession,
) -> Result<(), String> {
    let mut transaction = connection
        .begin()
        .await
        .map_err(|error| format!("Failed to begin authenticated session update: {error}"))?;
    persist_validated_on_connection(&mut transaction, validated).await?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit authenticated session update: {error}"))
}

pub(crate) async fn consume_signout_transition_and_clear_session_for_connection(
    connection: &mut SqliteConnection,
) -> Result<(), String> {
    let session_json = read_raw_session_item_for_connection(connection, SUPABASE_SESSION_KEY)
        .await?
        .ok_or_else(|| "No authenticated session is awaiting removal".to_string())?;
    let proof = read_proof_for_connection(connection)
        .await?
        .ok_or_else(|| "Authenticated session has no host proof".to_string())?;
    validate_snapshot_parts(&session_json, &proof, unix_now(), true)?;
    let transition = read_signout_transition_for_connection(connection)
        .await?
        .ok_or_else(|| {
            "Refusing to remove an authenticated session before durable sign-out completes"
                .to_string()
        })?;
    validate_transition(&transition, &proof)?;
    let mut transaction = connection
        .begin()
        .await
        .map_err(|error| format!("Failed to begin authenticated session removal: {error}"))?;
    let consumed = sqlx::query(
        "DELETE FROM auth_signout_transition
         WHERE singleton = ? AND user_id = ? AND session_sha256 = ? AND proof_generation = ?",
    )
    .bind(PROOF_SINGLETON)
    .bind(&transition.user_id)
    .bind(&transition.session_sha256)
    .bind(&transition.proof_generation)
    .execute(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to consume sign-out transition capability: {error}"))?;
    if consumed.rows_affected() != 1 {
        return Err("Sign-out transition capability was already consumed".into());
    }
    sqlx::query("DELETE FROM auth_principal_proof WHERE singleton = ?")
        .bind(PROOF_SINGLETON)
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to remove authenticated-principal proof: {error}"))?;
    sqlx::query("DELETE FROM auth_session WHERE key = ?")
        .bind(SUPABASE_SESSION_KEY)
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to remove Supabase session: {error}"))?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit authenticated session removal: {error}"))
}

/// Authorize a renderer-requested session replacement without trusting either
/// session's `user` object. A same-principal token renewal is safe; a different
/// principal is accepted only after the old account's durable wipe marked the
/// exact old proof ready for sign-out.
pub(crate) async fn session_replacement_kind_for_connection(
    connection: &mut SqliteConnection,
    new_principal: &VerifiedPrincipal,
) -> Result<SessionReplacementKind, String> {
    let session_json =
        read_raw_session_item_for_connection(connection, SUPABASE_SESSION_KEY).await?;
    let proof = read_proof_for_connection(connection).await?;
    match (session_json, proof) {
        (None, None) => Ok(SessionReplacementKind::IdentityTransition),
        (None, Some(_)) => Err("Authenticated-principal proof has no matching session".into()),
        (Some(session_json), None) => {
            // Legacy upgrade only: compare the new server-confirmed identity to
            // the old JWT subject, but never arm or route from that old claim.
            let envelope = parse_session(&session_json)?;
            let claims = parse_and_validate_claims(&envelope.access_token, unix_now(), true)?;
            validate_user_hint(&envelope, &claims.sub)?;
            if claims.sub == new_principal.user_id {
                Ok(SessionReplacementKind::IdentityTransition)
            } else {
                Err("A different persisted account must complete durable sign-out before replacement"
                    .into())
            }
        }
        (Some(session_json), Some(proof)) => {
            validate_snapshot_parts(&session_json, &proof, unix_now(), true)?;
            let transition = read_signout_transition_for_connection(connection).await?;
            if proof.user_id == new_principal.user_id && transition.is_none() {
                Ok(SessionReplacementKind::CredentialRefresh)
            } else if proof.user_id != new_principal.user_id {
                let transition = transition.ok_or_else(|| {
                    "A different authenticated account must complete durable sign-out before replacement"
                        .to_string()
                })?;
                validate_transition(&transition, &proof)?;
                Ok(SessionReplacementKind::IdentityTransition)
            } else {
                Err("Authenticated sign-out is already pending; finish it before installing another session"
                    .into())
            }
        }
    }
}

/// Called only after the app-database wipe commits while sign-out still holds
/// StateDb's sole connection. The persisted capability is bound to the exact
/// principal, session hash, and proof generation and can be consumed once.
#[cfg(test)]
async fn arm_signout_transition_for_test(
    connection: &mut SqliteConnection,
    expected_principal: &str,
) -> Result<(), String> {
    let snapshot = load_verified_snapshot_for_connection(connection, unix_now(), true)
        .await?
        .ok_or_else(|| "Authenticated session disappeared before sign-out completed".to_string())?;
    if snapshot.proof.user_id != expected_principal {
        return Err("Authenticated principal changed before sign-out completed".into());
    }
    sqlx::query(
        "INSERT INTO auth_signout_transition (
            singleton, user_id, session_sha256, proof_generation, issued_at
         ) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(singleton) DO UPDATE SET
            user_id = excluded.user_id,
            session_sha256 = excluded.session_sha256,
            proof_generation = excluded.proof_generation,
            issued_at = excluded.issued_at",
    )
    .bind(PROOF_SINGLETON)
    .bind(expected_principal)
    .bind(&snapshot.proof.session_sha256)
    .bind(&snapshot.proof.proof_generation)
    .bind(unix_now())
    .execute(connection)
    .await
    .map_err(|error| format!("Failed to mark authenticated session signed out: {error}"))?;
    Ok(())
}

/// Recover the cross-database commit point after a crash or a failed StateDb
/// write. The app DB transaction itself is the journal: only a fully committed
/// wipe leaves admission armed, closed, out of maintenance/remote-write mode,
/// and still bound to the wiped principal. A matching proof is required before
/// the one-shot StateDb transition can be armed.
pub async fn recover_committed_signout(
    app_pool: &SqlitePool,
    state_connection: &mut SqliteConnection,
) -> Result<bool, String> {
    let committed_principal: Option<String> = sqlx::query_scalar(
        "SELECT active_uid FROM auth_write_admission
         WHERE singleton = 1 AND armed = 1 AND accepting = 0
           AND maintenance = 0 AND remote_writes = 0 AND active_uid IS NOT NULL",
    )
    .fetch_optional(app_pool)
    .await
    .map_err(|error| format!("Failed to inspect durable sign-out commit state: {error}"))?
    .flatten();

    let Some(committed_principal) = committed_principal else {
        if read_signout_transition_for_connection(state_connection)
            .await?
            .is_some()
        {
            return Err(
                "Sign-out transition exists without a matching committed app-database wipe".into(),
            );
        }
        return Ok(false);
    };

    let session_json = read_raw_session_item_for_connection(state_connection, SUPABASE_SESSION_KEY)
        .await?
        .ok_or_else(|| {
            "Committed sign-out has no matching persisted Supabase session".to_string()
        })?;
    let proof = read_proof_for_connection(state_connection)
        .await?
        .ok_or_else(|| "Committed sign-out has no matching host proof".to_string())?;
    validate_snapshot_parts(&session_json, &proof, unix_now(), true)?;
    if proof.user_id != committed_principal {
        return Err(
            "Committed sign-out principal does not match the authenticated host proof".into(),
        );
    }

    if let Some(transition) = read_signout_transition_for_connection(state_connection).await? {
        validate_transition(&transition, &proof)?;
        return Ok(true);
    }

    let transition = SignoutTransition {
        user_id: proof.user_id.clone(),
        session_sha256: proof.session_sha256.clone(),
        proof_generation: proof.proof_generation.clone(),
        issued_at: unix_now(),
    };
    persist_signout_transition_on_connection(state_connection, &transition).await?;
    Ok(true)
}

pub(crate) async fn capture_auth_state_for_connection(
    connection: &mut SqliteConnection,
) -> Result<AuthStateBackup, String> {
    Ok(AuthStateBackup {
        session_json: read_raw_session_item_for_connection(connection, SUPABASE_SESSION_KEY)
            .await?,
        proof: read_proof_for_connection(connection).await?,
        transition: read_signout_transition_for_connection(connection).await?,
    })
}

/// Restore a pre-switch StateDb snapshot atomically. App-database admission
/// must remain closed until this succeeds and the previous verified principal
/// is re-armed by the command layer.
pub(crate) async fn restore_auth_state_for_connection(
    connection: &mut SqliteConnection,
    backup: &AuthStateBackup,
) -> Result<(), String> {
    let mut transaction = connection
        .begin()
        .await
        .map_err(|error| format!("Failed to begin authenticated session rollback: {error}"))?;
    sqlx::query("DELETE FROM auth_signout_transition WHERE singleton = ?")
        .bind(PROOF_SINGLETON)
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to clear sign-out transition during rollback: {error}"))?;
    sqlx::query("DELETE FROM auth_principal_proof WHERE singleton = ?")
        .bind(PROOF_SINGLETON)
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to clear principal proof during rollback: {error}"))?;
    sqlx::query("DELETE FROM auth_session WHERE key = ?")
        .bind(SUPABASE_SESSION_KEY)
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to clear session during rollback: {error}"))?;
    if let Some(session_json) = backup.session_json.as_deref() {
        sqlx::query("INSERT INTO auth_session (key, value) VALUES (?, ?)")
            .bind(SUPABASE_SESSION_KEY)
            .bind(session_json)
            .execute(&mut *transaction)
            .await
            .map_err(|error| format!("Failed to restore session during rollback: {error}"))?;
    }
    if let Some(proof) = backup.proof.as_ref() {
        persist_proof_on_connection(&mut transaction, proof).await?;
    }
    if let Some(transition) = backup.transition.as_ref() {
        persist_signout_transition_on_connection(&mut transaction, transition).await?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit authenticated session rollback: {error}"))
}

pub async fn load_verified_principal(
    pool: &SqlitePool,
) -> Result<Option<VerifiedPrincipal>, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to read authenticated session: {error}"))?;
    load_verified_principal_for_connection_at(&mut connection, unix_now()).await
}

pub async fn load_verified_principal_for_connection(
    connection: &mut SqliteConnection,
) -> Result<Option<VerifiedPrincipal>, String> {
    load_verified_principal_for_connection_at(connection, unix_now()).await
}

/// Renderer storage may still need the exact session bytes to ask Supabase to
/// revoke a session after the local wipe committed. A pending transition never
/// yields a principal, so this read cannot reopen app-database admission.
pub(crate) async fn load_renderer_session_for_connection(
    connection: &mut SqliteConnection,
) -> Result<Option<(String, Option<VerifiedPrincipal>)>, String> {
    let session_json =
        read_raw_session_item_for_connection(connection, SUPABASE_SESSION_KEY).await?;
    let proof = read_proof_for_connection(connection).await?;
    match (session_json, proof) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err("Authenticated-principal proof has no matching session".into()),
        (Some(_), None) => {
            Err("Persisted Supabase session has no host-authenticated principal proof".into())
        }
        (Some(session_json), Some(proof)) => {
            validate_snapshot_parts(&session_json, &proof, unix_now(), true)?;
            match read_signout_transition_for_connection(connection).await? {
                Some(transition) => {
                    validate_transition(&transition, &proof)?;
                    Ok(Some((session_json, None)))
                }
                None => {
                    // A normal renderer session is subject to exact expiry.
                    validate_snapshot_parts(&session_json, &proof, unix_now(), false)?;
                    Ok(Some((session_json, Some(proof.principal()))))
                }
            }
        }
    }
}

pub async fn get_current_user_id(pool: &SqlitePool) -> Result<Option<String>, String> {
    Ok(load_verified_principal(pool)
        .await?
        .map(|principal| principal.user_id))
}

/// Return a token/principal pair from one verified snapshot. Refresh is
/// serialized by StateDb's single connection and the old proof is compared
/// before the new session can replace it.
pub async fn get_current_auth(pool: &SqlitePool) -> Result<Option<VerifiedAuth>, String> {
    let server = SupabaseAuthServer::new()?;
    get_current_auth_with(pool, &server, unix_now()).await
}

pub async fn get_current_access_token(pool: &SqlitePool) -> Result<Option<String>, String> {
    Ok(get_current_auth(pool).await?.map(|auth| auth.access_token))
}

async fn load_or_bootstrap_verified_session(pool: &SqlitePool) -> Result<Option<String>, String> {
    let server = SupabaseAuthServer::new()?;
    load_or_bootstrap_verified_session_with(pool, &server, unix_now()).await
}

async fn load_or_bootstrap_verified_session_with(
    pool: &SqlitePool,
    server: &dyn AuthServer,
    now: i64,
) -> Result<Option<String>, String> {
    let (raw_session, proof_exists, transition_exists) =
        read_raw_session_and_proof_presence(pool).await?;
    let Some(raw_session) = raw_session else {
        if proof_exists || transition_exists {
            return Err("Authenticated auth state has no matching session".into());
        }
        return Ok(None);
    };
    if transition_exists && !proof_exists {
        return Err("Sign-out transition has no matching authenticated proof".into());
    }

    if proof_exists {
        let mut connection = pool
            .acquire()
            .await
            .map_err(|error| format!("Failed to read authenticated session: {error}"))?;
        let proof = read_proof_for_connection(&mut connection)
            .await?
            .ok_or_else(|| "Authenticated session proof disappeared while loading".to_string())?;
        validate_snapshot_parts(&raw_session, &proof, now, true)?;
        if let Some(transition) = read_signout_transition_for_connection(&mut connection).await? {
            validate_transition(&transition, &proof)?;
            return Ok(Some(raw_session));
        }
        drop(connection);
        // Refresh proven sessions through the same exact-proof CAS used by
        // backend callers. No network is touched while the token has more
        // than the refresh window remaining.
        get_current_auth_with(pool, server, now)
            .await?
            .ok_or_else(|| "Authenticated session disappeared while loading".to_string())?;
        let mut connection = pool
            .acquire()
            .await
            .map_err(|error| format!("Failed to read authenticated session: {error}"))?;
        let renderer_session = load_renderer_session_for_connection(&mut connection)
            .await?
            .ok_or_else(|| "Authenticated session disappeared while loading".to_string())?;
        return Ok(Some(renderer_session.0));
    }

    // Proof absence is the one legacy migration case. Never derive identity
    // from the old `user` object: authenticate its token with Supabase first.
    let legacy_envelope = parse_session(&raw_session)?;
    let legacy_claims = parse_and_validate_claims(&legacy_envelope.access_token, now, true)?;
    validate_user_hint(&legacy_envelope, &legacy_claims.sub)?;
    let validated = if legacy_claims.exp <= now {
        let refresh_token = legacy_envelope.refresh_token.as_deref().ok_or_else(|| {
            "Persisted legacy session is expired and has no refresh token".to_string()
        })?;
        let refreshed_json = server.refresh(refresh_token).await?;
        validate_session_with(&refreshed_json, server, now).await?
    } else {
        let authenticated_user_id = server
            .authenticated_user_id(&legacy_envelope.access_token)
            .await?;
        validated_session(
            &raw_session,
            legacy_envelope,
            legacy_claims,
            authenticated_user_id,
            now,
        )?
    };

    let installed_json = validated.session_json.clone();
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to lock legacy session bootstrap: {error}"))?;
    let current =
        read_raw_session_item_for_connection(&mut connection, SUPABASE_SESSION_KEY).await?;
    let current_proof_exists = proof_exists_for_connection(&mut connection).await?;
    if current.as_deref() != Some(raw_session.as_str()) || current_proof_exists {
        return Err(
            "Authenticated session changed during legacy validation; retry the read".into(),
        );
    }
    replace_session_for_connection(&mut connection, &validated).await?;
    Ok(Some(installed_json))
}

async fn get_current_auth_with(
    pool: &SqlitePool,
    server: &dyn AuthServer,
    now: i64,
) -> Result<Option<VerifiedAuth>, String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to read authenticated session: {error}"))?;
    let snapshot = load_verified_snapshot_for_connection(&mut connection, now, true).await?;
    drop(connection);

    let Some(snapshot) = snapshot else {
        return Ok(None);
    };
    if snapshot.proof.jwt_expires_at > now + REFRESH_WINDOW_SECONDS {
        return Ok(Some(snapshot.auth()));
    }

    let refresh_token =
        snapshot.envelope.refresh_token.as_deref().ok_or_else(|| {
            "Authenticated session is expiring and has no refresh token".to_string()
        })?;
    let refreshed_json = server.refresh(refresh_token).await?;
    let validated = validate_session_with(&refreshed_json, server, now).await?;
    if validated.proof.user_id != snapshot.proof.user_id {
        return Err(
            "Supabase refresh changed the authenticated principal; session was not replaced".into(),
        );
    }

    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to lock refreshed session: {error}"))?;
    let current = load_verified_snapshot_for_connection(&mut connection, now, true)
        .await?
        .ok_or_else(|| "Authenticated session disappeared during refresh".to_string())?;
    if current.session_json != snapshot.session_json || current.proof != snapshot.proof {
        return Err("Authenticated session changed during refresh; retry the operation".into());
    }
    replace_session_for_connection(&mut connection, &validated).await?;
    Ok(Some(VerifiedAuth {
        principal: validated.proof.principal(),
        access_token: validated.envelope.access_token,
    }))
}

impl VerifiedSnapshot {
    fn auth(&self) -> VerifiedAuth {
        VerifiedAuth {
            principal: self.proof.principal(),
            access_token: self.envelope.access_token.clone(),
        }
    }
}

async fn validate_session_with(
    session_json: &str,
    server: &dyn AuthServer,
    now: i64,
) -> Result<ValidatedSession, String> {
    let envelope = parse_session(session_json)?;
    let claims = parse_and_validate_claims(&envelope.access_token, now, false)?;
    validate_user_hint(&envelope, &claims.sub)?;
    // `/auth/v1/user` verifies the JWT with Supabase Auth. Only after that
    // authenticated response succeeds do these parsed claims become a durable
    // offline proof bound to the exact token hash.
    let authenticated_user_id = server.authenticated_user_id(&envelope.access_token).await?;
    validated_session(session_json, envelope, claims, authenticated_user_id, now)
}

fn validated_session(
    session_json: &str,
    envelope: SessionEnvelope,
    claims: JwtClaims,
    authenticated_user_id: String,
    now: i64,
) -> Result<ValidatedSession, String> {
    if authenticated_user_id != claims.sub {
        return Err("Supabase user id does not match the access token subject".into());
    }

    Ok(ValidatedSession {
        session_json: session_json.to_string(),
        proof: PrincipalProof {
            user_id: authenticated_user_id,
            session_sha256: sha256(session_json.as_bytes()),
            access_token_sha256: sha256(envelope.access_token.as_bytes()),
            jwt_sub: claims.sub,
            jwt_issuer: claims.iss,
            jwt_audience_json: serde_json::to_string(&claims.aud)
                .map_err(|error| format!("Failed to bind JWT audience: {error}"))?,
            jwt_expires_at: claims.exp,
            verified_at: now,
            proof_generation: uuid::Uuid::new_v4().to_string(),
        },
        envelope,
    })
}

fn validate_user_hint(envelope: &SessionEnvelope, jwt_subject: &str) -> Result<(), String> {
    if let Some(hinted_id) = envelope.user.as_ref().and_then(|user| user.id.as_deref()) {
        if hinted_id != jwt_subject {
            return Err("Session user id does not match the access token subject".into());
        }
    }
    Ok(())
}

async fn load_verified_principal_for_connection_at(
    connection: &mut SqliteConnection,
    now: i64,
) -> Result<Option<VerifiedPrincipal>, String> {
    Ok(
        load_verified_snapshot_for_connection(connection, now, false)
            .await?
            .map(|snapshot| snapshot.proof.principal()),
    )
}

async fn load_verified_snapshot_for_connection(
    connection: &mut SqliteConnection,
    now: i64,
    allow_expired_for_refresh: bool,
) -> Result<Option<VerifiedSnapshot>, String> {
    let session_json =
        read_raw_session_item_for_connection(connection, SUPABASE_SESSION_KEY).await?;
    let proof = read_proof_for_connection(connection).await?;
    match (session_json, proof) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err("Authenticated-principal proof has no matching session".into()),
        (Some(_), None) => {
            Err("Persisted Supabase session has no host-authenticated principal proof".into())
        }
        (Some(session_json), Some(proof)) => {
            let envelope =
                validate_snapshot_parts(&session_json, &proof, now, allow_expired_for_refresh)?;
            if let Some(transition) = read_signout_transition_for_connection(connection).await? {
                validate_transition(&transition, &proof)?;
                return Err(
                    "Authenticated session has completed its durable wipe and is awaiting removal"
                        .into(),
                );
            }
            Ok(Some(VerifiedSnapshot {
                session_json,
                envelope,
                proof,
            }))
        }
    }
}

fn validate_snapshot_parts(
    session_json: &str,
    proof: &PrincipalProof,
    now: i64,
    allow_expired_for_refresh: bool,
) -> Result<SessionEnvelope, String> {
    if sha256(session_json.as_bytes()) != proof.session_sha256 {
        return Err("Persisted Supabase session does not match its host proof".into());
    }
    let envelope = parse_session(session_json)?;
    if sha256(envelope.access_token.as_bytes()) != proof.access_token_sha256 {
        return Err("Persisted access token does not match its host proof".into());
    }
    let claims = parse_and_validate_claims(&envelope.access_token, now, allow_expired_for_refresh)?;
    let audience_json = serde_json::to_string(&claims.aud)
        .map_err(|error| format!("Failed to compare JWT audience: {error}"))?;
    if proof.user_id != proof.jwt_sub
        || proof.jwt_sub != claims.sub
        || proof.jwt_issuer != claims.iss
        || proof.jwt_audience_json != audience_json
        || proof.jwt_expires_at != claims.exp
    {
        return Err("Persisted access token claims do not match their host proof".into());
    }
    Ok(envelope)
}

async fn persist_validated_on_connection(
    connection: &mut SqliteConnection,
    validated: &ValidatedSession,
) -> Result<(), String> {
    // Any successful proof generation change invalidates an older transition
    // capability in the same StateDb transaction.
    sqlx::query("DELETE FROM auth_signout_transition WHERE singleton = ?")
        .bind(PROOF_SINGLETON)
        .execute(&mut *connection)
        .await
        .map_err(|error| format!("Failed to invalidate old sign-out transition: {error}"))?;
    sqlx::query(
        "INSERT INTO auth_session (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(SUPABASE_SESSION_KEY)
    .bind(&validated.session_json)
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("Failed to store Supabase session: {error}"))?;
    persist_proof_on_connection(connection, &validated.proof).await
}

async fn persist_proof_on_connection(
    connection: &mut SqliteConnection,
    proof: &PrincipalProof,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO auth_principal_proof (
            singleton, user_id, session_sha256, access_token_sha256,
            jwt_sub, jwt_issuer, jwt_audience_json, jwt_expires_at, verified_at,
            proof_generation
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(singleton) DO UPDATE SET
            user_id = excluded.user_id,
            session_sha256 = excluded.session_sha256,
            access_token_sha256 = excluded.access_token_sha256,
            jwt_sub = excluded.jwt_sub,
            jwt_issuer = excluded.jwt_issuer,
            jwt_audience_json = excluded.jwt_audience_json,
            jwt_expires_at = excluded.jwt_expires_at,
            verified_at = excluded.verified_at,
            proof_generation = excluded.proof_generation",
    )
    .bind(PROOF_SINGLETON)
    .bind(&proof.user_id)
    .bind(&proof.session_sha256)
    .bind(&proof.access_token_sha256)
    .bind(&proof.jwt_sub)
    .bind(&proof.jwt_issuer)
    .bind(&proof.jwt_audience_json)
    .bind(proof.jwt_expires_at)
    .bind(proof.verified_at)
    .bind(&proof.proof_generation)
    .execute(&mut *connection)
    .await
    .map_err(|error| format!("Failed to store authenticated-principal proof: {error}"))?;
    Ok(())
}

async fn read_raw_session_and_proof_presence(
    pool: &SqlitePool,
) -> Result<(Option<String>, bool, bool), String> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to read persisted session: {error}"))?;
    let session =
        read_raw_session_item_for_connection(&mut connection, SUPABASE_SESSION_KEY).await?;
    let proof = proof_exists_for_connection(&mut connection).await?;
    let transition = read_signout_transition_for_connection(&mut connection)
        .await?
        .is_some();
    Ok((session, proof, transition))
}

async fn read_raw_session_item_for_connection(
    connection: &mut SqliteConnection,
    key: &str,
) -> Result<Option<String>, String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM auth_session WHERE key = ?")
        .bind(key)
        .fetch_optional(connection)
        .await
        .map_err(|error| format!("Failed to read session value: {error}"))
}

async fn proof_exists_for_connection(connection: &mut SqliteConnection) -> Result<bool, String> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM auth_principal_proof WHERE singleton = ?")
            .bind(PROOF_SINGLETON)
            .fetch_one(connection)
            .await
            .map_err(|error| format!("Failed to inspect authenticated-principal proof: {error}"))?;
    Ok(count == 1)
}

async fn read_proof_for_connection(
    connection: &mut SqliteConnection,
) -> Result<Option<PrincipalProof>, String> {
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            String,
            String,
            i64,
            i64,
            String,
        ),
    >(
        "SELECT user_id, session_sha256, access_token_sha256, jwt_sub,
                jwt_issuer, jwt_audience_json, jwt_expires_at, verified_at, proof_generation
         FROM auth_principal_proof WHERE singleton = ?",
    )
    .bind(PROOF_SINGLETON)
    .fetch_optional(connection)
    .await
    .map_err(|error| format!("Failed to read authenticated-principal proof: {error}"))?;
    Ok(row.map(
        |(
            user_id,
            session_sha256,
            access_token_sha256,
            jwt_sub,
            jwt_issuer,
            jwt_audience_json,
            jwt_expires_at,
            verified_at,
            proof_generation,
        )| PrincipalProof {
            user_id,
            session_sha256,
            access_token_sha256,
            jwt_sub,
            jwt_issuer,
            jwt_audience_json,
            jwt_expires_at,
            verified_at,
            proof_generation,
        },
    ))
}

async fn read_signout_transition_for_connection(
    connection: &mut SqliteConnection,
) -> Result<Option<SignoutTransition>, String> {
    let row = sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT user_id, session_sha256, proof_generation, issued_at
         FROM auth_signout_transition WHERE singleton = ?",
    )
    .bind(PROOF_SINGLETON)
    .fetch_optional(connection)
    .await
    .map_err(|error| format!("Failed to read sign-out transition capability: {error}"))?;
    Ok(row.map(
        |(user_id, session_sha256, proof_generation, issued_at)| SignoutTransition {
            user_id,
            session_sha256,
            proof_generation,
            issued_at,
        },
    ))
}

async fn persist_signout_transition_on_connection(
    connection: &mut SqliteConnection,
    transition: &SignoutTransition,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO auth_signout_transition (
            singleton, user_id, session_sha256, proof_generation, issued_at
         ) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(PROOF_SINGLETON)
    .bind(&transition.user_id)
    .bind(&transition.session_sha256)
    .bind(&transition.proof_generation)
    .bind(transition.issued_at)
    .execute(connection)
    .await
    .map_err(|error| format!("Failed to restore sign-out transition capability: {error}"))?;
    Ok(())
}

fn validate_transition(
    transition: &SignoutTransition,
    proof: &PrincipalProof,
) -> Result<(), String> {
    if transition.user_id != proof.user_id
        || transition.session_sha256 != proof.session_sha256
        || transition.proof_generation != proof.proof_generation
    {
        return Err(
            "Stale sign-out transition does not match the current authenticated proof".into(),
        );
    }
    Ok(())
}

async fn get_raw_session_item(pool: &SqlitePool, key: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM auth_session WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|error| format!("Failed to read session value: {error}"))
}

async fn set_raw_session_item(pool: &SqlitePool, key: &str, value: &str) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO auth_session (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .map_err(|error| format!("Failed to store session value: {error}"))?;
    Ok(())
}

fn parse_session(session_json: &str) -> Result<SessionEnvelope, String> {
    if session_json.len() > MAX_SESSION_JSON_BYTES {
        return Err("Supabase session exceeds the host storage limit".into());
    }
    let session: SessionEnvelope = serde_json::from_str(session_json)
        .map_err(|error| format!("Failed to parse Supabase session: {error}"))?;
    if session.access_token.is_empty() || session.access_token.len() > MAX_ACCESS_TOKEN_BYTES {
        return Err("Supabase session has an invalid access token length".into());
    }
    Ok(session)
}

fn parse_and_validate_claims(
    access_token: &str,
    now: i64,
    allow_expired_for_refresh: bool,
) -> Result<JwtClaims, String> {
    let mut segments = access_token.split('.');
    let _header = segments.next();
    let payload = segments
        .next()
        .ok_or_else(|| "Supabase access token is not a JWT".to_string())?;
    let _signature = segments
        .next()
        .ok_or_else(|| "Supabase access token is not a signed JWT".to_string())?;
    if segments.next().is_some() || payload.is_empty() {
        return Err("Supabase access token has an invalid JWT shape".into());
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| "Supabase access token has invalid JWT encoding".to_string())?;
    let claims: JwtClaims = serde_json::from_slice(&decoded)
        .map_err(|_| "Supabase access token is missing required JWT claims".to_string())?;
    if claims.sub.trim().is_empty() {
        return Err("Supabase access token has an empty subject".into());
    }
    let expected_issuer = format!("{}/auth/v1", SUPABASE_URL.trim_end_matches('/'));
    if claims.iss != expected_issuer {
        return Err("Supabase access token has an unexpected issuer".into());
    }
    if !claims.aud.contains(AUTHENTICATED_AUDIENCE) {
        return Err("Supabase access token is not for an authenticated user".into());
    }
    if !allow_expired_for_refresh && claims.exp <= now {
        return Err("Supabase access token has expired".into());
    }
    Ok(claims)
}

fn sha256(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn bounded_response_detail(value: &str) -> String {
    const LIMIT: usize = 512;
    let value = value.trim().replace(['\n', '\r'], " ");
    if value.chars().count() <= LIMIT {
        value
    } else {
        format!("{}…", value.chars().take(LIMIT).collect::<String>())
    }
}

/// Explicit host-only identity setup for Rust tests. It never exists as a
/// renderer command and deliberately creates the same proof shape production
/// reads enforce.
#[cfg(test)]
pub(crate) async fn install_test_principal(pool: &SqlitePool, user_id: &str) -> Result<(), String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let expires_at = 4_102_444_800_i64;
    let payload = serde_json::json!({
        "sub": user_id,
        "iss": format!("{}/auth/v1", SUPABASE_URL.trim_end_matches('/')),
        "aud": AUTHENTICATED_AUDIENCE,
        "exp": expires_at,
    });
    let access_token = format!(
        "{}.{}.host-test-signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(payload.to_string())
    );
    let session_json = serde_json::json!({
        "access_token": access_token,
        "refresh_token": "host-test-refresh",
        "user": { "id": user_id },
    })
    .to_string();
    let envelope = parse_session(&session_json)?;
    let claims = parse_and_validate_claims(&envelope.access_token, 0, false)?;
    let validated = ValidatedSession {
        session_json: session_json.clone(),
        proof: PrincipalProof {
            user_id: user_id.to_string(),
            session_sha256: sha256(session_json.as_bytes()),
            access_token_sha256: sha256(envelope.access_token.as_bytes()),
            jwt_sub: claims.sub,
            jwt_issuer: claims.iss,
            jwt_audience_json: serde_json::to_string(&claims.aud)
                .map_err(|error| format!("Failed to bind test JWT audience: {error}"))?,
            jwt_expires_at: claims.exp,
            verified_at: 0,
            proof_generation: uuid::Uuid::new_v4().to_string(),
        },
        envelope,
    };
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to lock test identity: {error}"))?;
    replace_session_for_connection(&mut connection, &validated).await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;

    const NOW: i64 = 1_800_000_000;

    struct FakeAuthServer {
        users: Mutex<HashMap<String, Result<String, String>>>,
        refreshes: Mutex<HashMap<String, Result<String, String>>>,
    }

    impl FakeAuthServer {
        fn new() -> Self {
            Self {
                users: Mutex::new(HashMap::new()),
                refreshes: Mutex::new(HashMap::new()),
            }
        }

        fn accept(&self, token: &str, user_id: &str) {
            self.users
                .lock()
                .unwrap()
                .insert(token.to_string(), Ok(user_id.to_string()));
        }

        fn reject(&self, token: &str) {
            self.users.lock().unwrap().insert(
                token.to_string(),
                Err("server rejected forged token".into()),
            );
        }

        fn refresh_to(&self, refresh_token: &str, session_json: &str) {
            self.refreshes
                .lock()
                .unwrap()
                .insert(refresh_token.to_string(), Ok(session_json.to_string()));
        }
    }

    #[async_trait]
    impl AuthServer for FakeAuthServer {
        async fn authenticated_user_id(&self, token: &str) -> Result<String, String> {
            self.users
                .lock()
                .unwrap()
                .get(token)
                .cloned()
                .unwrap_or_else(|| Err("unexpected token".into()))
        }

        async fn refresh(&self, refresh_token: &str) -> Result<String, String> {
            self.refreshes
                .lock()
                .unwrap()
                .get(refresh_token)
                .cloned()
                .unwrap_or_else(|| Err("unexpected refresh token".into()))
        }
    }

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        initialize_auth_state_schema(&pool).await.unwrap();
        pool
    }

    async fn app_admission_pool(active_uid: &str, accepting: bool) -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE auth_write_admission (
                singleton INTEGER PRIMARY KEY,
                armed INTEGER NOT NULL,
                accepting INTEGER NOT NULL,
                maintenance INTEGER NOT NULL,
                remote_writes INTEGER NOT NULL,
                active_uid TEXT,
                generation INTEGER NOT NULL
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO auth_write_admission (
                singleton, armed, accepting, maintenance, remote_writes, active_uid, generation
             ) VALUES (1, 1, ?, 0, 0, ?, 7)",
        )
        .bind(i64::from(accepting))
        .bind(active_uid)
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn jwt(user_id: &str, expires_at: i64) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            json!({
                "sub": user_id,
                "iss": format!("{}/auth/v1", SUPABASE_URL),
                "aud": "authenticated",
                "exp": expires_at,
            })
            .to_string(),
        );
        format!("{header}.{payload}.signature")
    }

    fn session(user_id: &str, token: &str, refresh_token: &str) -> String {
        json!({
            "access_token": token,
            "refresh_token": refresh_token,
            "user": { "id": user_id },
        })
        .to_string()
    }

    async fn install_with(
        pool: &SqlitePool,
        server: &FakeAuthServer,
        session_json: &str,
    ) -> Result<VerifiedPrincipal, String> {
        let validated = validate_session_with(session_json, server, NOW).await?;
        let principal = validated.principal();
        let mut connection = pool.acquire().await.unwrap();
        replace_session_for_connection(&mut connection, &validated).await?;
        Ok(principal)
    }

    #[tokio::test]
    async fn forged_renderer_user_never_becomes_a_principal() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let token = jwt("attacker", NOW + 600);
        server.accept(&token, "attacker");
        let forged = session("victim", &token, "refresh");

        let error = validate_session_with(&forged, &server, NOW)
            .await
            .err()
            .unwrap();
        assert!(error.contains("does not match"));
        assert_eq!(load_verified_principal(&pool).await.unwrap(), None);
    }

    #[tokio::test]
    async fn server_rejection_cannot_replace_an_existing_identity() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let original_token = jwt("original", NOW + 600);
        server.accept(&original_token, "original");
        install_with(
            &pool,
            &server,
            &session("original", &original_token, "original-refresh"),
        )
        .await
        .unwrap();

        let forged_token = jwt("victim", NOW + 600);
        server.reject(&forged_token);
        assert!(validate_session_with(
            &session("victim", &forged_token, "forged-refresh"),
            &server,
            NOW
        )
        .await
        .is_err());
        assert_eq!(
            load_verified_principal_for_connection_at(&mut *pool.acquire().await.unwrap(), NOW)
                .await
                .unwrap()
                .unwrap()
                .user_id,
            "original"
        );
    }

    #[tokio::test]
    async fn token_or_session_mutation_fails_closed() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let token = jwt("real", NOW + 600);
        server.accept(&token, "real");
        install_with(&pool, &server, &session("real", &token, "refresh"))
            .await
            .unwrap();

        let replacement = session("other", &jwt("other", NOW + 600), "refresh");
        sqlx::query("UPDATE auth_session SET value = ? WHERE key = ?")
            .bind(replacement)
            .bind(SUPABASE_SESSION_KEY)
            .execute(&pool)
            .await
            .unwrap();

        let error =
            load_verified_principal_for_connection_at(&mut *pool.acquire().await.unwrap(), NOW)
                .await
                .err()
                .unwrap();
        assert!(error.contains("does not match"));
    }

    #[tokio::test]
    async fn expired_proof_fails_closed_without_network() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let token = jwt("real", NOW + 10);
        server.accept(&token, "real");
        install_with(&pool, &server, &session("real", &token, "refresh"))
            .await
            .unwrap();

        let error = load_verified_principal_for_connection_at(
            &mut *pool.acquire().await.unwrap(),
            NOW + 11,
        )
        .await
        .err()
        .unwrap();
        assert!(error.contains("expired"));
    }

    #[tokio::test]
    async fn legacy_session_bootstraps_only_from_authenticated_server_response() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let token = jwt("legacy", NOW + 600);
        let legacy = session("legacy", &token, "refresh");
        set_raw_session_item(&pool, SUPABASE_SESSION_KEY, &legacy)
            .await
            .unwrap();
        server.accept(&token, "legacy");

        assert_eq!(
            load_or_bootstrap_verified_session_with(&pool, &server, NOW)
                .await
                .unwrap(),
            Some(legacy)
        );
        assert_eq!(
            load_verified_principal_for_connection_at(&mut *pool.acquire().await.unwrap(), NOW)
                .await
                .unwrap()
                .unwrap()
                .user_id,
            "legacy"
        );
    }

    #[tokio::test]
    async fn expired_legacy_session_refreshes_then_proves_the_new_token() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let expired_token = jwt("legacy", NOW - 1);
        let renewed_token = jwt("legacy", NOW + 3_600);
        let legacy = session("legacy", &expired_token, "legacy-refresh");
        let renewed = session("legacy", &renewed_token, "renewed-refresh");
        set_raw_session_item(&pool, SUPABASE_SESSION_KEY, &legacy)
            .await
            .unwrap();
        server.refresh_to("legacy-refresh", &renewed);
        server.accept(&renewed_token, "legacy");

        assert_eq!(
            load_or_bootstrap_verified_session_with(&pool, &server, NOW)
                .await
                .unwrap(),
            Some(renewed.clone())
        );
        assert_eq!(
            get_raw_session_item(&pool, SUPABASE_SESSION_KEY)
                .await
                .unwrap(),
            Some(renewed)
        );
        assert_eq!(
            load_verified_principal_for_connection_at(&mut *pool.acquire().await.unwrap(), NOW)
                .await
                .unwrap()
                .unwrap()
                .user_id,
            "legacy"
        );
    }

    #[tokio::test]
    async fn unverified_legacy_session_is_not_read_as_guest_or_principal() {
        let pool = test_pool().await;
        let token = jwt("forged", NOW + 600);
        set_raw_session_item(
            &pool,
            SUPABASE_SESSION_KEY,
            &session("victim", &token, "refresh"),
        )
        .await
        .unwrap();

        let error =
            load_verified_principal_for_connection_at(&mut *pool.acquire().await.unwrap(), NOW)
                .await
                .err()
                .unwrap();
        assert!(error.contains("no host-authenticated"));
    }

    #[tokio::test]
    async fn refresh_revalidates_and_atomically_rebinds_the_new_token() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let old_token = jwt("real", NOW + 30);
        let new_token = jwt("real", NOW + 3_600);
        let old_session = session("real", &old_token, "old-refresh");
        let new_session = session("real", &new_token, "new-refresh");
        server.accept(&old_token, "real");
        server.accept(&new_token, "real");
        server.refresh_to("old-refresh", &new_session);
        install_with(&pool, &server, &old_session).await.unwrap();

        let auth = get_current_auth_with(&pool, &server, NOW)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(auth.principal.user_id, "real");
        assert_eq!(auth.access_token, new_token);

        let stored = get_raw_session_item(&pool, SUPABASE_SESSION_KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, new_session);
        let snapshot =
            load_verified_snapshot_for_connection(&mut *pool.acquire().await.unwrap(), NOW, false)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            snapshot.proof.access_token_sha256,
            sha256(new_token.as_bytes())
        );
    }

    #[tokio::test]
    async fn startup_storage_read_refreshes_an_expired_proven_session() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let old_token = jwt("real", NOW - 1);
        let new_token = jwt("real", NOW + 3_600);
        let old_session = session("real", &old_token, "old-refresh");
        let new_session = session("real", &new_token, "new-refresh");
        server.accept(&old_token, "real");
        server.accept(&new_token, "real");
        server.refresh_to("old-refresh", &new_session);
        let originally_valid = validate_session_with(&old_session, &server, NOW - 120)
            .await
            .unwrap();
        replace_session_for_connection(&mut *pool.acquire().await.unwrap(), &originally_valid)
            .await
            .unwrap();

        assert_eq!(
            load_or_bootstrap_verified_session_with(&pool, &server, NOW)
                .await
                .unwrap(),
            Some(new_session)
        );
        assert_eq!(
            load_verified_principal_for_connection_at(&mut *pool.acquire().await.unwrap(), NOW)
                .await
                .unwrap()
                .unwrap()
                .expires_at,
            NOW + 3_600
        );
    }

    #[tokio::test]
    async fn auth_state_backup_restores_session_and_proof_together() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let original_token = jwt("original", NOW + 600);
        let replacement_token = jwt("replacement", NOW + 600);
        server.accept(&original_token, "original");
        server.accept(&replacement_token, "replacement");
        install_with(
            &pool,
            &server,
            &session("original", &original_token, "original-refresh"),
        )
        .await
        .unwrap();

        let mut connection = pool.acquire().await.unwrap();
        let backup = capture_auth_state_for_connection(&mut connection)
            .await
            .unwrap();
        let replacement = validate_session_with(
            &session("replacement", &replacement_token, "replacement-refresh"),
            &server,
            NOW,
        )
        .await
        .unwrap();
        persist_validated_session_unchecked(&mut connection, &replacement)
            .await
            .unwrap();
        restore_auth_state_for_connection(&mut connection, &backup)
            .await
            .unwrap();

        assert_eq!(
            load_verified_principal_for_connection_at(&mut connection, NOW)
                .await
                .unwrap()
                .unwrap()
                .user_id,
            "original"
        );
    }

    #[tokio::test]
    async fn refresh_cannot_switch_principals() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let old_token = jwt("real", NOW + 30);
        let attacker_token = jwt("attacker", NOW + 3_600);
        let old_session = session("real", &old_token, "old-refresh");
        let attacker_session = session("attacker", &attacker_token, "attacker-refresh");
        server.accept(&old_token, "real");
        server.accept(&attacker_token, "attacker");
        server.refresh_to("old-refresh", &attacker_session);
        install_with(&pool, &server, &old_session).await.unwrap();

        let error = get_current_auth_with(&pool, &server, NOW)
            .await
            .err()
            .unwrap();
        assert!(error.contains("changed the authenticated principal"));
        assert_eq!(
            get_raw_session_item(&pool, SUPABASE_SESSION_KEY)
                .await
                .unwrap()
                .unwrap(),
            old_session
        );
    }

    #[tokio::test]
    async fn renderer_cannot_remove_a_session_before_durable_signout() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let token = jwt("alice", NOW + 600);
        server.accept(&token, "alice");
        install_with(&pool, &server, &session("alice", &token, "refresh"))
            .await
            .unwrap();

        let error = consume_signout_transition_and_clear_session_for_connection(
            &mut *pool.acquire().await.unwrap(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("durable sign-out"), "{error}");
        assert_eq!(
            load_verified_principal_for_connection_at(&mut *pool.acquire().await.unwrap(), NOW)
                .await
                .unwrap()
                .unwrap()
                .user_id,
            "alice"
        );
    }

    #[tokio::test]
    async fn cross_principal_replacement_requires_a_matching_transition() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let alice_token = jwt("alice", NOW + 600);
        let bob_token = jwt("bob", NOW + 600);
        server.accept(&alice_token, "alice");
        server.accept(&bob_token, "bob");
        install_with(
            &pool,
            &server,
            &session("alice", &alice_token, "alice-refresh"),
        )
        .await
        .unwrap();
        let bob = validate_session_with(&session("bob", &bob_token, "bob-refresh"), &server, NOW)
            .await
            .unwrap();

        let error = replace_session_for_connection(&mut *pool.acquire().await.unwrap(), &bob)
            .await
            .unwrap_err();
        assert!(error.contains("durable sign-out"), "{error}");
    }

    #[tokio::test]
    async fn same_principal_token_replacement_needs_no_transition() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let old_token = jwt("alice", NOW + 600);
        let new_token = jwt("alice", NOW + 3_600);
        server.accept(&old_token, "alice");
        server.accept(&new_token, "alice");
        install_with(&pool, &server, &session("alice", &old_token, "old-refresh"))
            .await
            .unwrap();
        let renewed =
            validate_session_with(&session("alice", &new_token, "new-refresh"), &server, NOW)
                .await
                .unwrap();

        let mut connection = pool.acquire().await.unwrap();
        assert_eq!(
            session_replacement_kind_for_connection(&mut connection, &renewed.principal())
                .await
                .unwrap(),
            SessionReplacementKind::CredentialRefresh
        );
        replace_session_for_connection(&mut connection, &renewed)
            .await
            .unwrap();
        drop(connection);
        let auth = get_current_auth_with(&pool, &server, NOW)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(auth.principal.user_id, "alice");
        assert_eq!(auth.access_token, new_token);
    }

    #[tokio::test]
    async fn armed_transition_allows_one_removal_and_replay_is_denied() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let token = jwt("alice", NOW + 600);
        server.accept(&token, "alice");
        install_with(&pool, &server, &session("alice", &token, "refresh"))
            .await
            .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        arm_signout_transition_for_test(&mut connection, "alice")
            .await
            .unwrap();
        consume_signout_transition_and_clear_session_for_connection(&mut connection)
            .await
            .unwrap();

        let replay = consume_signout_transition_and_clear_session_for_connection(&mut connection)
            .await
            .unwrap_err();
        assert!(replay.contains("No authenticated session"), "{replay}");
    }

    #[tokio::test]
    async fn armed_transition_allows_one_cross_principal_replacement() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let alice_token = jwt("alice", NOW + 600);
        let bob_token = jwt("bob", NOW + 600);
        server.accept(&alice_token, "alice");
        server.accept(&bob_token, "bob");
        install_with(
            &pool,
            &server,
            &session("alice", &alice_token, "alice-refresh"),
        )
        .await
        .unwrap();
        let bob = validate_session_with(&session("bob", &bob_token, "bob-refresh"), &server, NOW)
            .await
            .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        arm_signout_transition_for_test(&mut connection, "alice")
            .await
            .unwrap();
        assert_eq!(
            session_replacement_kind_for_connection(&mut connection, &bob.principal())
                .await
                .unwrap(),
            SessionReplacementKind::IdentityTransition
        );
        replace_session_for_connection(&mut connection, &bob)
            .await
            .unwrap();
        assert!(read_signout_transition_for_connection(&mut connection)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            load_verified_principal_for_connection_at(&mut connection, NOW)
                .await
                .unwrap()
                .unwrap()
                .user_id,
            "bob"
        );
    }

    #[tokio::test]
    async fn transition_is_invalid_after_proof_generation_changes() {
        let pool = test_pool().await;
        let server = FakeAuthServer::new();
        let token = jwt("alice", NOW + 600);
        server.accept(&token, "alice");
        install_with(&pool, &server, &session("alice", &token, "refresh"))
            .await
            .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        arm_signout_transition_for_test(&mut connection, "alice")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE auth_principal_proof SET proof_generation = 'different-generation'
             WHERE singleton = 1",
        )
        .execute(&mut *connection)
        .await
        .unwrap();

        let error = consume_signout_transition_and_clear_session_for_connection(&mut connection)
            .await
            .unwrap_err();
        assert!(error.contains("Stale sign-out transition"), "{error}");
    }

    #[tokio::test]
    async fn pending_signout_rollback_restores_the_committed_wipe_journal() {
        let state_pool = test_pool().await;
        let app_pool = app_admission_pool("alice", false).await;
        let server = FakeAuthServer::new();
        let alice_token = jwt("alice", NOW + 600);
        let bob_token = jwt("bob", NOW + 600);
        server.accept(&alice_token, "alice");
        server.accept(&bob_token, "bob");
        install_with(
            &state_pool,
            &server,
            &session("alice", &alice_token, "alice-refresh"),
        )
        .await
        .unwrap();

        let mut state_connection = state_pool.acquire().await.unwrap();
        assert!(recover_committed_signout(&app_pool, &mut state_connection)
            .await
            .unwrap());
        let state_backup = capture_auth_state_for_connection(&mut state_connection)
            .await
            .unwrap();
        let admission_backup = capture_write_admission(&app_pool, &mut state_connection)
            .await
            .unwrap();

        // This is the command-level failure window: admission was closed and
        // the pending A transition was consumed to install B, but arming B
        // failed. Rollback must restore both A's transition and its app-DB
        // crash journal, not flatten the journal into anonymous closed state.
        suspend_write_admission(&app_pool, &admission_backup)
            .await
            .unwrap();
        let bob = validate_session_with(&session("bob", &bob_token, "bob-refresh"), &server, NOW)
            .await
            .unwrap();
        replace_session_for_connection(&mut state_connection, &bob)
            .await
            .unwrap();
        restore_auth_state_for_connection(&mut state_connection, &state_backup)
            .await
            .unwrap();
        restore_write_admission(&app_pool, &admission_backup)
            .await
            .unwrap();

        let admission: (i64, i64, i64, i64, Option<String>, i64) = sqlx::query_as(
            "SELECT armed, accepting, maintenance, remote_writes, active_uid, generation
             FROM auth_write_admission WHERE singleton = 1",
        )
        .fetch_one(&app_pool)
        .await
        .unwrap();
        assert_eq!(admission, (1, 0, 0, 0, Some("alice".into()), 9));
        assert!(recover_committed_signout(&app_pool, &mut state_connection)
            .await
            .unwrap());
        assert!(load_renderer_session_for_connection(&mut state_connection)
            .await
            .unwrap()
            .unwrap()
            .1
            .is_none());
    }

    #[tokio::test]
    async fn bootstrap_failure_closes_new_identity_before_restoring_previous_one() {
        let state_pool = test_pool().await;
        let app_pool = app_admission_pool("alice", false).await;
        let server = FakeAuthServer::new();
        let alice_token = jwt("alice", NOW + 600);
        let bob_token = jwt("bob", NOW + 600);
        server.accept(&alice_token, "alice");
        server.accept(&bob_token, "bob");
        install_with(
            &state_pool,
            &server,
            &session("alice", &alice_token, "alice-refresh"),
        )
        .await
        .unwrap();

        let mut state_connection = state_pool.acquire().await.unwrap();
        arm_signout_transition_for_test(&mut state_connection, "alice")
            .await
            .unwrap();
        let state_backup = capture_auth_state_for_connection(&mut state_connection)
            .await
            .unwrap();
        let previous_admission = capture_write_admission(&app_pool, &mut state_connection)
            .await
            .unwrap();
        suspend_write_admission(&app_pool, &previous_admission)
            .await
            .unwrap();

        let bob = validate_session_with(&session("bob", &bob_token, "bob-refresh"), &server, NOW)
            .await
            .unwrap();
        replace_session_for_connection(&mut state_connection, &bob)
            .await
            .unwrap();
        let activated = arm_write_admission_for_identity_switch(&app_pool, Some("bob"))
            .await
            .unwrap();

        // Simulate a codec/bootstrap error after Bob was admitted. The old
        // generation token can no longer restore Alice until Bob's exact gate
        // is first CAS-closed and its newer closed token is presented.
        assert!(restore_write_admission(&app_pool, &previous_admission)
            .await
            .is_err());
        let closed = suspend_write_admission_for_rollback(&app_pool, &activated)
            .await
            .unwrap();
        restore_auth_state_for_connection(&mut state_connection, &state_backup)
            .await
            .unwrap();
        restore_write_admission_from_closed(&app_pool, &previous_admission, &closed)
            .await
            .unwrap();

        drop(state_connection);
        assert!(load_verified_principal(&state_pool)
            .await
            .unwrap_err()
            .contains("awaiting removal"));
        let mut restored_state = state_pool.acquire().await.unwrap();
        let restored_renderer = load_renderer_session_for_connection(&mut restored_state)
            .await
            .unwrap()
            .unwrap();
        assert!(restored_renderer.0.contains("alice"));
        assert!(restored_renderer.1.is_none());
        drop(restored_state);
        let admission: (i64, i64, i64, i64, Option<String>, i64) = sqlx::query_as(
            "SELECT armed, accepting, maintenance, remote_writes, active_uid, generation
             FROM auth_write_admission WHERE singleton = 1",
        )
        .fetch_one(&app_pool)
        .await
        .unwrap();
        assert_eq!(admission, (1, 0, 0, 0, Some("alice".into()), 11));
    }

    #[tokio::test]
    async fn restart_recovers_committed_wipe_without_rearming_the_principal() {
        let state_pool = test_pool().await;
        let app_pool = app_admission_pool("alice", false).await;
        let server = FakeAuthServer::new();
        let token = jwt("alice", NOW + 600);
        server.accept(&token, "alice");
        install_with(&state_pool, &server, &session("alice", &token, "refresh"))
            .await
            .unwrap();

        let mut restarted_state = state_pool.acquire().await.unwrap();
        assert!(recover_committed_signout(&app_pool, &mut restarted_state)
            .await
            .unwrap());
        assert!(
            load_verified_principal_for_connection_at(&mut restarted_state, NOW)
                .await
                .unwrap_err()
                .contains("awaiting removal")
        );
        let renderer = load_renderer_session_for_connection(&mut restarted_state)
            .await
            .unwrap()
            .unwrap();
        assert!(renderer.1.is_none());
        assert!(recover_committed_signout(&app_pool, &mut restarted_state)
            .await
            .unwrap());
    }
}
