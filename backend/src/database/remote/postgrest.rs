//! Calling a Postgres function over PostgREST as the signed-in user.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::common::SyncError;

/// The PostgREST endpoint and the key it wants beside a user's token.
pub struct Postgrest {
    http: reqwest::Client,
    url: String,
    anon_key: String,
}

impl Postgrest {
    /// The endpoint this build talks to — see [`crate::config::postgrest_url`].
    #[must_use]
    pub fn from_config() -> Self {
        Self::new(
            crate::config::postgrest_url(),
            crate::config::supabase_anon_key(),
        )
    }

    #[must_use]
    pub fn new(url: String, anon_key: String) -> Self {
        Self {
            // A request a person is waiting on: fail in seconds rather than
            // spin for a minute on a dead network.
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(8))
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
            url,
            anon_key,
        }
    }

    /// `POST /rpc/<function>` with named `args`, decoded as `T`.
    ///
    /// # Errors
    ///
    /// [`SyncError::RequestFailed`] when the server could not be reached, and
    /// [`SyncError::ApiError`] with PostgREST's JSON body when it answered with
    /// anything but success.
    pub async fn rpc<T: DeserializeOwned>(
        &self,
        function: &str,
        args: &Value,
        token: &str,
    ) -> Result<T, SyncError> {
        let response = self
            .http
            .post(format!("{}/rpc/{function}", self.url.trim_end_matches('/')))
            .header("apikey", &self.anon_key)
            .header("Authorization", format!("Bearer {token}"))
            .json(args)
            .send()
            .await
            .map_err(|error| SyncError::RequestFailed(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| SyncError::RequestFailed(error.to_string()))?;
        if !status.is_success() {
            return Err(SyncError::ApiError {
                status: status.as_u16(),
                message: body,
            });
        }
        serde_json::from_str(body.trim()).map_err(|error| SyncError::ApiError {
            status: status.as_u16(),
            message: format!("unexpected {function} response {body:?}: {error}"),
        })
    }
}
