//! Writing a Supabase session, and the host proof that binds it, into a
//! fixture's `state.db`.
//!
//! Sessions here are hand-built rather than obtained. `/auth/v1/user` is what
//! mints a real proof, so a fixture that went through production code would
//! need Supabase on the other end — and would still have no way to produce the
//! *unproven* case, which is a session with no proof beside it. The proof's
//! whole contract is local (two hashes over exact bytes, plus the claims
//! parsed out of them), so writing one directly is faithful.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::Digest as _;

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
    /// turn this into an identity, and boot does not make one — so this is the
    /// lapsed session that raises the sign-in gate.
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
fn access_token(expires_at: i64) -> String {
    let encode = |value: &Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(value).expect("claims serialize"))
    };
    let header = encode(&json!({ "alg": "HS256", "typ": "JWT" }));
    let payload = encode(&json!({
        "sub": PRINCIPAL,
        "iss": ISSUER,
        "aud": ["authenticated"],
        "exp": expires_at,
    }));
    format!("{header}.{payload}.signature")
}

/// The `uid` rows must carry to be visible to this fixture's launch, or `None`
/// for the guest namespace.
pub fn owner(stored: Stored) -> Option<&'static str> {
    match stored {
        Stored::Proven { .. } => Some(PRINCIPAL),
        Stored::Unproven => None,
    }
}

/// Write `stored` into the state database at `dir`, opening and closing it.
pub async fn seed(dir: &Path, stored: Stored) {
    let state = luma_lib::database::local::state::init_state_db_at(dir)
        .await
        .expect("failed to open the fixture state database");

    let (expires_at, refresh_token) = match stored {
        // A refresh token Supabase has already spent. Reaching for it is the
        // failure the sign-in suite exists to keep out of boot.
        Stored::Proven { expires_in } => (now() + expires_in, "already-used-refresh-token"),
        Stored::Unproven => (now() + 3600, "unproven"),
    };
    let session = serde_json::to_string(&json!({
        "access_token": access_token(expires_at),
        "refresh_token": refresh_token,
        "token_type": "bearer",
        "expires_at": expires_at,
        "user": { "id": PRINCIPAL },
    }))
    .expect("session serializes");

    sqlx::query("INSERT INTO auth_session (key, value) VALUES ('supabase_session', ?)")
        .bind(&session)
        .execute(&state.0)
        .await
        .expect("failed to seed the session");

    if let Stored::Proven { .. } = stored {
        let envelope: Value = serde_json::from_str(&session).expect("session parses");
        sqlx::query(
            "INSERT INTO auth_principal_proof (
                singleton, user_id, session_sha256, access_token_sha256,
                jwt_sub, jwt_issuer, jwt_audience_json, jwt_expires_at,
                verified_at, proof_generation
             ) VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(PRINCIPAL)
        .bind(sha256(&session))
        .bind(sha256(envelope["access_token"].as_str().expect("a token")))
        .bind(PRINCIPAL)
        .bind(ISSUER)
        .bind(r#"["authenticated"]"#)
        .bind(expires_at)
        .bind(now())
        .bind("fixture-proof-generation")
        .execute(&state.0)
        .await
        .expect("failed to seed the principal proof");
    }

    state.0.close().await;
}
