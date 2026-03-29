//! `RemoteClient` implementation backed by `SupabaseClient`.

use async_trait::async_trait;
use serde_json::Value;

use super::error::SyncError;
use super::traits::RemoteClient;
use crate::database::remote::common::SupabaseClient;

fn convert_err(e: crate::database::remote::common::SyncError) -> SyncError {
    use crate::database::remote::common::SyncError as OldErr;
    match e {
        OldErr::RequestFailed(msg) => SyncError::Network(msg),
        OldErr::ApiError { status, message } => SyncError::Api { status, message },
        OldErr::ParseError(msg) => SyncError::Parse(msg),
        OldErr::MissingField(field) => SyncError::MissingField(field),
    }
}

#[async_trait]
impl RemoteClient for SupabaseClient {
    async fn select_json(
        &self,
        table: &str,
        query: &str,
        token: &str,
    ) -> Result<Vec<Value>, SyncError> {
        self.select(table, query, token).await.map_err(convert_err)
    }

    async fn upsert_json(
        &self,
        table: &str,
        payload: &Value,
        conflict_key: &str,
        token: &str,
    ) -> Result<(), SyncError> {
        self.upsert_no_return(table, payload, conflict_key, token)
            .await
            .map_err(convert_err)
    }

    async fn upload_file(
        &self,
        bucket: &str,
        path: &str,
        bytes: Vec<u8>,
        content_type: &str,
        token: &str,
    ) -> Result<String, SyncError> {
        SupabaseClient::upload_file(self, bucket, path, bytes, content_type, token)
            .await
            .map_err(convert_err)
    }

    async fn download_file(
        &self,
        bucket: &str,
        path: &str,
        token: &str,
    ) -> Result<Vec<u8>, SyncError> {
        SupabaseClient::download_file(self, bucket, path, token)
            .await
            .map_err(convert_err)
    }
}
