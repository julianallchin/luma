// Shared types and utilities for remote Supabase operations

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Error type for Supabase sync operations
#[derive(Debug)]
pub enum SyncError {
    /// HTTP request failed
    RequestFailed(String),
    /// Supabase API returned an error
    ApiError { status: u16, message: String },
    /// Failed to parse response
    ParseError(String),
    /// Missing required field
    MissingField(String),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncError::RequestFailed(msg) => write!(f, "Request failed: {}", msg),
            SyncError::ApiError { status, message } => {
                write!(f, "Supabase API error {}: {}", status, message)
            }
            SyncError::ParseError(msg) => write!(f, "Parse error: {}", msg),
            SyncError::MissingField(field) => write!(f, "Missing required field: {}", field),
        }
    }
}

impl std::error::Error for SyncError {}

/// Response wrapper for Supabase INSERT operations with RETURNING
#[derive(Debug, Deserialize)]
pub struct InsertResponse {
    pub id: i64,
}

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

    /// Insert a new record and return the generated ID
    pub async fn insert<T: Serialize>(
        &self,
        table: &str,
        payload: &T,
        access_token: &str,
    ) -> Result<i64, SyncError> {
        let url = format!("{}/rest/v1/{}?select=id", self.base_url, table);

        let res = self
            .client
            .post(&url)
            .header("apikey", &self.anon_key)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .header("Prefer", "return=representation")
            .json(payload)
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

        let body = res
            .text()
            .await
            .map_err(|e| SyncError::ParseError(e.to_string()))?;

        // Response is an array with one element
        let mut results: Vec<InsertResponse> = serde_json::from_str(&body)
            .map_err(|e| SyncError::ParseError(format!("Failed to parse response: {}", e)))?;

        results
            .pop()
            .map(|r| r.id)
            .ok_or_else(|| SyncError::ParseError("No ID returned from insert".to_string()))
    }

    /// Update an existing record by ID
    pub async fn update<T: Serialize>(
        &self,
        table: &str,
        id: i64,
        payload: &T,
        access_token: &str,
    ) -> Result<(), SyncError> {
        let url = format!("{}/rest/v1/{}?id=eq.{}", self.base_url, table, id);

        let res = self
            .client
            .patch(&url)
            .header("apikey", &self.anon_key)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .json(payload)
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

        Ok(())
    }

    /// Delete a record by ID
    pub async fn delete(&self, table: &str, id: i64, access_token: &str) -> Result<(), SyncError> {
        let url = format!("{}/rest/v1/{}?id=eq.{}", self.base_url, table, id);

        let res = self
            .client
            .delete(&url)
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

        Ok(())
    }
}
