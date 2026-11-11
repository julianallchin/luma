//! Writing a Supabase session, and the host proof that binds it, into a
//! fixture's `state.db`.
//!
//! Sessions here are hand-built rather than obtained. `/auth/v1/user` is what
//! mints a real proof, so a fixture that went through production code would
//! need Supabase on the other end — and would still have no way to produce the
//! *unproven* case, which is a session with no proof beside it. The proof's
//! whole contract is local (two hashes over exact bytes, plus the claims
//! parsed out of them), so writing one directly is faithful.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::Digest as _;
use sqlx::SqlitePool;

/// Must match `config::SUPABASE_URL`; the issuer claim is compared against it.
pub const ISSUER: &str = "https://smuuycypmsutwrkpctws.supabase.co/auth/v1";
/// The principal a proven fixture session names. Rows a signed-in test expects
/// to see must carry it as their `uid`.
pub const PRINCIPAL: &str = "11111111-2222-3333-4444-555555555555";

/// What a fixture's state database holds when the app opens it.
#[derive(Clone, Copy)]
pub enum Stored {
    /// A session and the host proof that binds it — the ordinary signed-in
    /// machine. `expires_in` may be negative: an access token past its `exp`
    /// is exactly the state that used to trigger a fatal refresh at boot.
    Proven { expires_in: i64 },
    /// Session bytes with no proof beside them. Only an online round trip can
    /// turn this into an identity; boot tries one, and this refresh token is
    /// one Supabase will not honour — so this is the lapsed session that
    /// raises the sign-in gate, after the splash.
    Unproven,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn sha256(value: &str) -> String {
    format!("sha256:{:x}", sha2::Sha256::digest(value.as_bytes()))
}

/// A JWT the host can parse. The signature is never checked — trust comes from
/// the proof row beside it — so only the payload has to be real.
fn access_token(principal: &str, expires_at: i64) -> String {
    let encode = |value: &Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(value).expect("claims serialize"))
    };
    let header = encode(&json!({ "alg": "HS256", "typ": "JWT" }));
    let payload = encode(&json!({
        "sub": principal,
        "iss": ISSUER,
        "aud": ["authenticated"],
        "exp": expires_at,
    }));
    format!("{header}.{payload}.signature")
}

/// Sign the fixture at `dir` in as [`PRINCIPAL`], the way every harness that
/// is not testing the gate itself launches: nothing in Luma opens without a
/// principal, so a fixture that skipped this would open on the sign-in screen.
///
/// Rows the fixture writes must carry [`PRINCIPAL`] as their `uid`, or the
/// principal will see none of them: visibility is `uid IS active_uid`, and an
/// authored row's identity is immutable once written.
pub async fn signed_in(dir: &Path) {
    seed(dir, Stored::Proven { expires_in: 3600 }).await;
}

/// [`signed_in`], as `principal`: a copy of a real library is signed in as
/// the account that owns its rows, with a session made here. Its own session
/// is never copied, and the harness keeps the library off the network.
pub async fn signed_in_as(dir: &Path, principal: &str) {
    seed_as(dir, Stored::Proven { expires_in: 3600 }, principal).await;
}

/// Write `stored` into the state database at `dir`, opening and closing it.
pub async fn seed(dir: &Path, stored: Stored) {
    seed_as(dir, stored, PRINCIPAL).await;
}

/// Mark `principal` as having been through the camera walkthrough, which a
/// score otherwise opens under — see `luma_app`'s `visualizer::tour`.
pub async fn toured(dir: &Path, principal: &str) {
    let state = luma_lib::database::local::state::init_state_db_at(dir)
        .await
        .expect("failed to open the fixture state database");
    sqlx::query("INSERT INTO auth_session (key, value) VALUES (?, 'true')")
        .bind(format!("camera-tour:{principal}"))
        .execute(&state.0)
        .await
        .expect("failed to seed the camera walkthrough");
    state.0.close().await;
}

async fn seed_as(dir: &Path, stored: Stored, principal: &str) {
    let state = luma_lib::database::local::state::init_state_db_at(dir)
        .await
        .expect("failed to open the fixture state database");
    write(&state.0, stored, principal)
        .await
        .expect("failed to seed the session");
    state.0.close().await;
}

/// The session, and for a proven one its proof, as rows of `state`.
async fn write(state: &SqlitePool, stored: Stored, principal: &str) -> Result<(), sqlx::Error> {
    let (expires_at, refresh_token) = match stored {
        // A refresh token Supabase has already spent. Reaching for it is the
        // failure the sign-in suite exists to keep out of boot.
        Stored::Proven { expires_in } => (now() + expires_in, "already-used-refresh-token"),
        Stored::Unproven => (now() + 3600, "unproven"),
    };
    let session = serde_json::to_string(&json!({
        "access_token": access_token(principal, expires_at),
        "refresh_token": refresh_token,
        "token_type": "bearer",
        "expires_at": expires_at,
        "user": { "id": principal },
    }))
    .expect("session serializes");

    sqlx::query("INSERT INTO auth_session (key, value) VALUES ('supabase_session', ?)")
        .bind(&session)
        .execute(state)
        .await?;

    if let Stored::Proven { .. } = stored {
        let envelope: Value = serde_json::from_str(&session).expect("session parses");
        sqlx::query(
            "INSERT INTO auth_principal_proof (
                singleton, user_id, session_sha256, access_token_sha256,
                jwt_sub, jwt_issuer, jwt_audience_json, jwt_expires_at,
                verified_at, proof_generation
             ) VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(principal)
        .bind(sha256(&session))
        .bind(sha256(envelope["access_token"].as_str().expect("a token")))
        .bind(principal)
        .bind(ISSUER)
        .bind(r#"["authenticated"]"#)
        .bind(expires_at)
        .bind(now())
        .bind("fixture-proof-generation")
        .execute(state)
        .await?;
    }
    Ok(())
}

/// The sign-in screen's far end, for a library that is already open: each
/// address in `accounts` signs in as the principal it names, and any other is
/// refused. No code is mailed and none is checked.
///
/// What a redeemed code does in the app is `set_session_item`, which asks
/// Supabase who the token belongs to. This writes the same rows without
/// asking, and keeps that command's rule about whose session may replace
/// whose: with nobody signed in the account is admitted; the account already
/// signed in — its session revoked, say — only has its credentials renewed;
/// and any other account is refused until that one has signed out.
#[must_use]
pub fn accounts(accounts: HashMap<String, String>) -> luma_app::SignInFixture {
    Arc::new(move |services, email| {
        let principal = accounts.get(email.trim()).cloned();
        Box::pin(async move {
            let principal = principal.ok_or("The fixture has no account with this address")?;
            let state = services.state_pool();
            let stored: Option<String> =
                sqlx::query_scalar("SELECT user_id FROM auth_principal_proof")
                    .fetch_optional(state)
                    .await
                    .map_err(|error| error.to_string())?;
            match stored {
                Some(stored) if stored == principal => {}
                Some(_) => {
                    return Err(
                        "A different authenticated account must complete durable sign-out before replacement"
                            .into(),
                    )
                }
                None => {
                    write(state, Stored::Proven { expires_in: 3600 }, &principal)
                        .await
                        .map_err(|error| error.to_string())?;
                    luma_lib::database::local::auth::arm_write_admission(
                        &services.db().0,
                        Some(&principal),
                    )
                    .await?;
                }
            }
            Ok(principal)
        })
    })
}
