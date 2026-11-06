//! The Supabase HTTP endpoints beside record replication: Storage for media
//! bytes, and PostgREST for the few calls that are a request and a response
//! (today, redeeming a venue's share code).

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::sync::error::SyncError;

/// Supabase Storage: the buckets that hold track audio, stems and figures.
pub struct Storage {
    client: reqwest::Client,
    base_url: String,
    anon_key: String,
}

impl Storage {
    pub fn new(base_url: String, anon_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            anon_key,
        }
    }

    /// Upload a file to Supabase Storage
    pub async fn upload_file(
        &self,
        bucket: &str,
        path: &str,
        file_bytes: Vec<u8>,
        content_type: &str,
        access_token: &str,
    ) -> Result<String, SyncError> {
        let url = format!("{}/storage/v1/object/{}/{}", self.base_url, bucket, path);

        let res = self
            .client
            .post(&url)
            .header("apikey", &self.anon_key)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", content_type)
            .header("x-upsert", "true")
            .body(file_bytes)
            .send()
            .await
            .map_err(|e| SyncError::Network(e.to_string()))?;

        if !res.status().is_success() {
            let status = res.status().as_u16();
            let text = res.text().await.unwrap_or_default();
            return Err(SyncError::Api {
                status,
                message: text,
            });
        }

        Ok(format!("{}/{}", bucket, path))
    }

    /// Download a file from Supabase Storage
    pub async fn download_file(
        &self,
        bucket: &str,
        path: &str,
        access_token: &str,
    ) -> Result<Vec<u8>, SyncError> {
        let url = format!("{}/storage/v1/object/{}/{}", self.base_url, bucket, path);

        let res = self
            .client
            .get(&url)
            .header("apikey", &self.anon_key)
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
            .map_err(|e| SyncError::Network(e.to_string()))?;

        if !res.status().is_success() {
            let status = res.status().as_u16();
            let text = res.text().await.unwrap_or_default();
            return Err(SyncError::Api {
                status,
                message: text,
            });
        }

        res.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| SyncError::Network(e.to_string()))
    }
}

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
    /// [`SyncError::Network`] when the server could not be reached, and
    /// [`SyncError::Api`] with PostgREST's JSON body when it answered with
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
            .map_err(|error| SyncError::Network(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| SyncError::Network(error.to_string()))?;
        if !status.is_success() {
            return Err(SyncError::Api {
                status: status.as_u16(),
                message: body,
            });
        }
        serde_json::from_str(body.trim()).map_err(|error| SyncError::Api {
            status: status.as_u16(),
            message: format!("unexpected {function} response {body:?}: {error}"),
        })
    }
}
