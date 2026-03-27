use async_trait::async_trait;
use serde_json::Value;

use super::error::SyncError;

/// Abstraction over the remote Supabase backend.
///
/// The real implementation wraps `SupabaseClient` from `database::remote::common`.
/// Tests swap in a `MockRemoteClient` with canned responses — no Supabase
/// instance needed.
#[async_trait]
pub trait RemoteClient: Send + Sync {
    /// SELECT rows from a PostgREST table.
    async fn select_json(
        &self,
        table: &str,
        query: &str,
        token: &str,
    ) -> Result<Vec<Value>, SyncError>;

    /// Upsert a single row (INSERT … ON CONFLICT merge).
    async fn upsert_json(
        &self,
        table: &str,
        payload: &Value,
        conflict_key: &str,
        token: &str,
    ) -> Result<(), SyncError>;

    /// Delete a single row by its `id` column.
    async fn delete(&self, table: &str, id: &str, token: &str) -> Result<(), SyncError>;

    /// Upload a file to Supabase Storage. Returns the storage path.
    async fn upload_file(
        &self,
        bucket: &str,
        path: &str,
        bytes: Vec<u8>,
        content_type: &str,
        token: &str,
    ) -> Result<String, SyncError>;

    /// Download a file from Supabase Storage.
    async fn download_file(
        &self,
        bucket: &str,
        path: &str,
        token: &str,
    ) -> Result<Vec<u8>, SyncError>;
}
