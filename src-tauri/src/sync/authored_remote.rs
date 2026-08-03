//! Typed boundary for the three server-authoritative authored-head RPCs.
//!
//! Immutable revisions travel through ordinary row sync. Mutable document
//! heads do not: devices submit ordered proposals, and any authenticated owner
//! device may resolve the earliest proposal. Sync integration never remains
//! blocked on a semantic conflict: the adapter either creates a structural or
//! whole-proposal merge revision, recognizes ancestry, or terminally
//! quarantines an unreadable proposal as a no-op. Typed user-facing conflicts
//! remain authored operation/turn outcomes, not sync state.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::error::SyncError;
use super::traits::RemoteClient;

pub const SUBMIT_HEAD_PROPOSAL_RPC: &str = "submit_authored_head_proposal";
pub const INTEGRATE_HEAD_PROPOSAL_RPC: &str = "integrate_authored_head_proposal";
pub const ARCHIVE_AUTHORED_DOCUMENT_RPC: &str = "archive_authored_document";

pub const SUBMIT_HEAD_PROPOSAL_OP: &str = "submit_authored_head_proposal";
pub const INTEGRATE_HEAD_PROPOSAL_OP: &str = "integrate_authored_head_proposal";
pub const ARCHIVE_AUTHORED_DOCUMENT_OP: &str = "archive_authored_document";

/// Narrow bridge between transport and the domain-aware total merge engine.
///
/// Push owns durable retry/removal of the wake-up. The implementation owns
/// loading the latest server/local revisions, producing any merge revision,
/// uploading its immutable closure, invoking the integration RPC, and applying
/// the terminal receipt locally. Keeping this behind a trait prevents the
/// transport queue from depending on a stale precomputed merge result.
#[async_trait::async_trait]
pub trait HeadProposalIntegrator: Send + Sync {
    async fn integrate_pending_proposal(
        &self,
        pool: &SqlitePool,
        remote: &dyn RemoteClient,
        token: &str,
        admitted_user_id: &str,
        proposal_id: &str,
    ) -> Result<HeadIntegrationReceipt, SyncError>;
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct SubmitHeadProposalInput {
    pub proposal_id: String,
    pub document_id: String,
    pub device_id: String,
    pub operation_id: String,
    pub base_revision_id: Option<String>,
    pub proposed_revision_id: String,
    /// Trusted-host audit time persisted in the local proposal row. The RPC
    /// stores this exact value so a later pull enriches, rather than collides
    /// with, the local immutable input.
    pub created_at: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum HeadProposalStatus {
    Pending,
    Integrated,
    QuarantinedNoop,
    CancelledArchived,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct HeadProposalReceipt {
    pub proposal_id: String,
    pub document_id: String,
    pub proposal_seq: i64,
    pub status: HeadProposalStatus,
    pub base_revision_id: Option<String>,
    pub proposed_revision_id: String,
    pub current_head_revision_id: Option<String>,
    pub is_earliest_pending: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum HeadIntegrationResolution {
    Structural,
    WholeProposal,
    QuarantinedNoop,
    FastForward,
    AlreadyAncestor,
    CancelledArchived,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct IntegrateHeadProposalInput {
    pub proposal_id: String,
    pub expected_head_revision_id: Option<String>,
    pub resolution: HeadIntegrationResolution,
    pub result_revision_id: Option<String>,
}

impl IntegrateHeadProposalInput {
    pub fn new(
        proposal_id: impl Into<String>,
        expected_head_revision_id: Option<String>,
        resolution: HeadIntegrationResolution,
        result_revision_id: impl Into<String>,
    ) -> Self {
        Self {
            proposal_id: proposal_id.into(),
            expected_head_revision_id,
            resolution,
            result_revision_id: Some(result_revision_id.into()),
        }
    }

    pub fn quarantined_noop(
        proposal_id: impl Into<String>,
        expected_head_revision_id: Option<String>,
    ) -> Self {
        Self {
            proposal_id: proposal_id.into(),
            result_revision_id: expected_head_revision_id.clone(),
            expected_head_revision_id,
            resolution: HeadIntegrationResolution::QuarantinedNoop,
        }
    }

    pub fn validate(&self) -> Result<(), SyncError> {
        if self.proposal_id.is_empty()
            || self
                .result_revision_id
                .as_deref()
                .is_some_and(str::is_empty)
        {
            return Err(SyncError::Parse(
                "authored-head integration requires non-empty revision ids".into(),
            ));
        }
        if self.resolution != HeadIntegrationResolution::QuarantinedNoop
            && self.result_revision_id.is_none()
        {
            return Err(SyncError::Parse(format!(
                "{:?} integration requires a result revision",
                self.resolution
            )));
        }
        Ok(())
    }
}

/// Transport outcome, distinct from the proposal's durable terminal status.
/// `Stale` asks the semantic adapter to recompute against `current_head`; it is
/// never silently treated as success or last-writer-wins.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum HeadIntegrationOutcome {
    Integrated,
    QuarantinedNoop,
    AlreadyResolved,
    Stale,
    NotEarliest,
    Archived,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HeadIntegrationReceipt {
    pub proposal_id: String,
    pub document_id: String,
    pub outcome: HeadIntegrationOutcome,
    pub proposal_status: HeadProposalStatus,
    pub current_head_revision_id: Option<String>,
    pub integrated_revision_id: Option<String>,
    pub resolution: Option<HeadIntegrationResolution>,
    pub integration_seq: Option<i64>,
    pub integrated_at: Option<String>,
}

impl HeadIntegrationReceipt {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.outcome,
            HeadIntegrationOutcome::Integrated
                | HeadIntegrationOutcome::QuarantinedNoop
                | HeadIntegrationOutcome::AlreadyResolved
                | HeadIntegrationOutcome::Archived
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct ArchiveAuthoredDocumentInput {
    pub archive_id: String,
    pub document_id: String,
    pub device_id: String,
    pub operation_id: String,
    /// The revision visible to the requesting device. `None` means that device
    /// had no local revision; the server may still capture a concurrently
    /// integrated head as the archive's final revision.
    pub requested_revision_id: Option<String>,
    /// Client-authored audit time. The server records its own `sync_seq` and
    /// captures the locked current head separately as `final_revision_id`.
    pub archived_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct ArchiveAuthoredDocumentReceipt {
    pub archive_id: String,
    pub document_id: String,
    pub status: String,
    pub final_revision_id: Option<String>,
    pub cancelled_proposal_count: i64,
    pub archive_seq: i64,
    pub document_archived_at: String,
}

pub async fn submit_head_proposal(
    remote: &dyn RemoteClient,
    input: &SubmitHeadProposalInput,
    token: &str,
) -> Result<HeadProposalReceipt, SyncError> {
    rpc(remote, SUBMIT_HEAD_PROPOSAL_RPC, input, token).await
}

pub async fn integrate_head_proposal(
    remote: &dyn RemoteClient,
    input: &IntegrateHeadProposalInput,
    token: &str,
) -> Result<HeadIntegrationReceipt, SyncError> {
    input.validate()?;
    rpc(remote, INTEGRATE_HEAD_PROPOSAL_RPC, input, token).await
}

pub async fn archive_authored_document(
    remote: &dyn RemoteClient,
    input: &ArchiveAuthoredDocumentInput,
    token: &str,
) -> Result<ArchiveAuthoredDocumentReceipt, SyncError> {
    rpc(remote, ARCHIVE_AUTHORED_DOCUMENT_RPC, input, token).await
}

async fn rpc<I, O>(
    remote: &dyn RemoteClient,
    function: &str,
    input: &I,
    token: &str,
) -> Result<O, SyncError>
where
    I: Serialize,
    O: for<'de> Deserialize<'de>,
{
    let payload =
        serde_json::to_value(input).map_err(|error| SyncError::Parse(error.to_string()))?;
    let result = remote.rpc_json(function, &payload, token).await?;
    serde_json::from_value(result)
        .map_err(|error| SyncError::Parse(format!("invalid response from {function}: {error}")))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::{json, Value};

    use super::*;

    struct RpcRemote {
        calls: Mutex<Vec<(String, Value, String)>>,
        responses: Mutex<Vec<Value>>,
    }

    impl RpcRemote {
        fn new(responses: Vec<Value>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into_iter().rev().collect()),
            }
        }
    }

    #[async_trait]
    impl RemoteClient for RpcRemote {
        async fn select_json(
            &self,
            _table: &str,
            _query: &str,
            _token: &str,
        ) -> Result<Vec<Value>, SyncError> {
            Ok(Vec::new())
        }

        async fn upsert_json(
            &self,
            _table: &str,
            _payload: &Value,
            _conflict_key: &str,
            _token: &str,
        ) -> Result<(), SyncError> {
            Ok(())
        }

        async fn patch_json(
            &self,
            _table: &str,
            _filter: &str,
            _payload: &Value,
            _token: &str,
        ) -> Result<(), SyncError> {
            Ok(())
        }

        async fn rpc_json(
            &self,
            function: &str,
            payload: &Value,
            token: &str,
        ) -> Result<Value, SyncError> {
            self.calls.lock().unwrap().push((
                function.to_owned(),
                payload.clone(),
                token.to_owned(),
            ));
            self.responses
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| SyncError::Parse("missing mock RPC response".into()))
        }

        async fn upload_file(
            &self,
            _bucket: &str,
            _path: &str,
            _bytes: Vec<u8>,
            _content_type: &str,
            _token: &str,
        ) -> Result<String, SyncError> {
            Ok(String::new())
        }

        async fn download_file(
            &self,
            _bucket: &str,
            _path: &str,
            _token: &str,
        ) -> Result<Vec<u8>, SyncError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn a_different_owner_client_can_integrate_an_offline_proposal() {
        let remote = RpcRemote::new(vec![json!({
            "proposal_id": "proposal-a",
            "document_id": "document",
            "outcome": "integrated",
            "proposal_status": "integrated",
            "current_head_revision_id": "merge",
            "integrated_revision_id": "merge",
            "resolution": "structural",
            "integration_seq": 7,
            "integrated_at": "2026-08-02T00:00:00Z"
        })]);
        let result = integrate_head_proposal(
            &remote,
            &IntegrateHeadProposalInput::new(
                "proposal-a",
                Some("remote-head".into()),
                HeadIntegrationResolution::Structural,
                "merge",
            ),
            "owner-client-b-token",
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, HeadIntegrationOutcome::Integrated);
        let calls = remote.calls.lock().unwrap();
        assert_eq!(calls[0].0, INTEGRATE_HEAD_PROPOSAL_RPC);
        assert_eq!(calls[0].2, "owner-client-b-token");
    }

    #[test]
    fn empty_result_cannot_be_submitted() {
        let input = IntegrateHeadProposalInput::new(
            "proposal",
            None,
            HeadIntegrationResolution::QuarantinedNoop,
            "",
        );
        assert!(input.validate().is_err());
    }

    #[test]
    fn stale_and_not_earliest_are_explicitly_nonterminal() {
        for outcome in [
            HeadIntegrationOutcome::Stale,
            HeadIntegrationOutcome::NotEarliest,
        ] {
            let receipt = HeadIntegrationReceipt {
                proposal_id: "proposal".into(),
                document_id: "document".into(),
                outcome,
                proposal_status: HeadProposalStatus::Pending,
                current_head_revision_id: Some("head".into()),
                integrated_revision_id: None,
                resolution: None,
                integration_seq: None,
                integrated_at: None,
            };
            assert!(!receipt.is_terminal());
        }
    }

    #[test]
    fn postgres_surface_has_exactly_three_public_rpcs_and_server_only_heads() {
        let migration =
            include_str!("../../../supabase/migrations/20260802000000_authored_revision_sync.sql");
        assert_eq!(
            migration
                .matches("CREATE OR REPLACE FUNCTION public.")
                .count(),
            3
        );
        for rpc in [
            SUBMIT_HEAD_PROPOSAL_RPC,
            INTEGRATE_HEAD_PROPOSAL_RPC,
            ARCHIVE_AUTHORED_DOCUMENT_RPC,
        ] {
            assert!(migration.contains(&format!("FUNCTION public.{rpc}(")));
            assert!(migration.contains(&format!("GRANT EXECUTE ON FUNCTION public.{rpc}(")));
        }
        assert!(migration.contains(
            "GRANT SELECT ON\n    public.authored_document_heads,\n    public.authored_head_proposals"
        ));
        assert!(!migration
            .contains("GRANT SELECT, INSERT, UPDATE ON\n    public.authored_document_heads"));
        assert_eq!(
            migration
                .matches("CREATE OR REPLACE FUNCTION private.")
                .count(),
            migration.matches("REVOKE ALL ON FUNCTION private.").count(),
            "every private helper must have its default execute privilege revoked"
        );
        assert_eq!(
            migration
                .matches("GRANT EXECUTE ON FUNCTION private.current_principal_key()")
                .count(),
            1,
            "only the RLS identity projection is re-granted"
        );
        assert!(
            !migration.contains("rpc."),
            "PL/pgSQL parameters must use positional aliases, not block-label qualification"
        );
        assert_eq!(migration.matches(" ALIAS FOR $").count(), 17);
        assert!(
            !migration.contains("DEFAULT private.next_sync_seq()"),
            "client inserts must not require EXECUTE on the private clock allocator"
        );
        assert!(migration.contains("CREATE TRIGGER immutable_assign_sync_seq BEFORE INSERT"));
        assert!(migration.contains("CREATE TRIGGER sync_seq_bump BEFORE INSERT OR UPDATE"));
    }

    #[test]
    fn every_new_postgres_table_has_exactly_one_server_cursor() {
        let migration =
            include_str!("../../../supabase/migrations/20260802000000_authored_revision_sync.sql");
        let mut tables = 0;
        for (offset, _) in migration.match_indices("CREATE TABLE public.") {
            let declaration = &migration[offset..];
            let block = declaration
                .split_once("\n);")
                .expect("CREATE TABLE declaration must be terminated")
                .0;
            let table = block
                .lines()
                .next()
                .expect("CREATE TABLE declaration has a first line");
            assert_eq!(
                block.matches("sync_seq bigint").count(),
                1,
                "{table} must declare exactly one commit-ordered cursor"
            );
            tables += 1;
        }
        assert_eq!(tables, 16);
    }

    #[test]
    fn postgres_convergence_contract_is_terminal_and_commit_ordered() {
        let migration =
            include_str!("../../../supabase/migrations/20260802000000_authored_revision_sync.sql");
        assert!(migration.contains("UPDATE private.luma_sync_clock"));
        assert!(migration.contains("ORDER BY pending.server_proposal_seq"));
        assert!(migration.contains(
            "PERFORM private.assert_revision_closure(\n            owner_key, input_document_id, input_proposed_revision_id"
        ));
        assert!(migration.contains("'whole_proposal', 'quarantined_noop'"));
        assert!(migration.contains("'cancelled_archived'"));
        assert!(migration.contains("structural result must merge current then proposal"));
        assert!(migration.contains("agent thread deletion does not match its owned authored route"));
        assert!(!migration.contains("set_config('luma."));
        assert!(
            migration.contains("authored archive transition requires its immutable archive fact")
        );
        assert!(migration.contains(
            "UPDATE public.scores\n           SET deleted_at = canonical_archive::timestamptz"
        ));
        assert!(migration.contains(
            "UPDATE public.patterns\n           SET deleted_at = canonical_archive::timestamptz"
        ));
        assert!(migration.contains("sibling.archived_at IS NULL"));
        assert!(migration.contains("CREATE TRIGGER preserve_catalog_tombstone BEFORE UPDATE"));
        assert!(migration.contains("new graph document conflicts with its catalog route"));
        assert_eq!(
            migration
                .matches("pg_advisory_xact_lock(hashtextextended(")
                .count(),
            2,
            "new-route insert and archive must share the same terminal-scope lock"
        );
        assert!(migration.contains("new graph document conflicts with a terminal authored route"));
        assert!(migration.contains("resolution_kind IN ('fast_forward', 'whole_proposal')"));
        assert!(migration.contains("Sibling implementation archives can be submitted concurrently"));
        assert!(migration.contains(
            "operation_kind IN (\n            'create_score', 'create_pattern', 'score_edit', 'graph_edit',\n            'restore', 'workspace_commit', 'workspace_merge', 'pattern_fork'"
        ));
    }

    #[test]
    fn postgres_document_identity_matches_rust_and_bounds_untrusted_routes() {
        let migration =
            include_str!("../../../supabase/migrations/20260802000000_authored_revision_sync.sql");
        assert!(migration.contains("CREATE OR REPLACE FUNCTION private.expected_document_id("));
        assert!(migration
            .contains("decode('6c756d612e617574686f7265642d646f63756d656e742e763100', 'hex')"));
        assert!(migration
            .contains("ad-5d78d30274abf38a2d9af6dab42ed7577eaf8b617e5847e8b792dcdd3d58eb94"));
        assert!(migration
            .contains("ad-a937e9175da900a0bb4522038184659e6f142a44690ef8cbc0be8f6b7cf494df"));
        assert!(
            migration.contains("NEW.document_id IS DISTINCT FROM private.expected_document_id(")
        );
        assert!(
            migration.contains("authored document route fields must contain 1 to 4096 UTF-8 bytes")
        );
        assert!(migration.contains("REVOKE ALL ON FUNCTION private.expected_document_id("));
        assert!(!migration.contains("implementation_name"));
    }

    #[test]
    fn postgres_prepare_and_headless_archive_contracts_match_local_state() {
        let migration =
            include_str!("../../../supabase/migrations/20260802000000_authored_revision_sync.sql");
        assert!(migration.contains("revision.operation_kind = 'agent_turn_prepare'"));
        assert!(migration.contains("revision.operation_id = NEW.assistant_message_id"));
        assert!(migration.contains("revision.assistant_message_id IS NULL"));
        assert!(migration.contains(
            "existing.requested_revision_id IS DISTINCT FROM input_requested_revision_id"
        ));
        assert!(migration.contains("IF input_requested_revision_id IS NOT NULL THEN"));
        assert!(migration
            .contains("not assert that the server is headless; final_revision_id captures the"));
        assert!(migration.contains("locked server head and may therefore be non-NULL"));
    }

    #[test]
    fn postgres_ingress_cannot_publish_rows_that_pin_sqlite_cursors() {
        let migration =
            include_str!("../../../supabase/migrations/20260802000000_authored_revision_sync.sql");

        assert!(migration.contains("private.guard_authored_revision_parent_insert()"));
        assert!(migration.contains("NEW.parent_order < revision.parent_count"));
        assert!(migration.contains("private.guard_authored_revision_file_insert()"));
        assert!(migration.contains("NEW.content_hash <> private.expected_file_hash(NEW.content)"));
        assert!(migration.contains("existing_file_count >= 4096"));
        assert!(migration.contains("existing_total_bytes + octet_length(NEW.content) > 67108864"));
        assert!(migration
            .contains("Existing paths consume no additional quota; let the immutable UPDATE"));
        assert!(migration.contains(
            "parts_json text NOT NULL CHECK (jsonb_typeof(parts_json::jsonb) = 'array')"
        ));
        assert!(migration
            .contains("conflicts_json IS NULL OR jsonb_typeof(conflicts_json::jsonb) = 'array'"));
        assert!(migration.contains("agent_kind = 'track_copilot'"));
        assert!(migration.contains("agent_kind = 'pattern_graph'"));
        assert!(migration.contains("result.operation_kind = 'agent_turn'"));
        assert!(migration.contains("parent.parent_order = 1"));
        assert!(migration.contains("private.assert_audit_token(value text, field_name text)"));
        assert!(migration.contains("value !~ '^[A-Za-z0-9_.:-]+$'"));
        assert!(migration.contains("private.assert_rfc3339(value text, field_name text)"));
        assert!(migration
            .contains("PERFORM private.assert_rfc3339(input_created_at, 'proposal created_at')"));
        assert!(migration
            .contains("PERFORM private.assert_rfc3339(input_archived_at, 'archive archived_at')"));
        assert!(migration.contains("private.guard_authored_revision_insert()"));
        assert!(migration.contains("NEW.operation_kind !~ '^[a-z0-9_]+$'"));
        assert!(migration.contains("octet_length(NEW.message) > 8192"));

        let revisions = migration
            .split_once("CREATE TABLE public.authored_revisions (")
            .unwrap()
            .1
            .split_once("\n);")
            .unwrap()
            .0;
        assert!(revisions.contains("operation_kind text NOT NULL"));
        assert!(
            !revisions.contains("operation_kind IN"),
            "revision history operation kinds must remain forward-compatible"
        );
    }
}
