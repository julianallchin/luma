// Shared types and utilities for remote Supabase operations

use reqwest::Client;
use std::fmt;

/// Error type for Supabase sync operations
#[derive(Debug)]
pub enum SyncError {
    /// HTTP request failed
    RequestFailed(String),
    /// Supabase API returned an error
    ApiError { status: u16, message: String },
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncError::RequestFailed(msg) => write!(f, "Request failed: {}", msg),
            SyncError::ApiError { status, message } => {
                write!(f, "Supabase API error {}: {}", status, message)
            }
        }
    }
}

impl std::error::Error for SyncError {}

/// Supabase client configuration
pub struct SupabaseClient {
    client: Client,
    base_url: String,
    anon_key: String,
}

impl SupabaseClient {
    /// Create a new Supabase client
    pub fn new(base_url: String, anon_key: String) -> Self {
        Self {
            client: Client::new(),
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
            .map_err(|e| SyncError::RequestFailed(e.to_string()))?;

        if !res.status().is_success() {
            let status = res.status().as_u16();
            let text = res.text().await.unwrap_or_default();
            return Err(SyncError::ApiError {
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
            .map_err(|e| SyncError::RequestFailed(e.to_string()))?;

        if !res.status().is_success() {
            let status = res.status().as_u16();
            let text = res.text().await.unwrap_or_default();
            return Err(SyncError::ApiError {
                status,
                message: text,
            });
        }

        res.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| SyncError::RequestFailed(e.to_string()))
    }
}
