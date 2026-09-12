//! Uploads: the PowerSync CRUD queue drained into Supabase PostgREST.
//!
//! A local write becomes a row in `ps_crud` (see `triggers.rs`) and this
//! connector replays it against PostgREST as the signed-in user, so Postgres
//! row-level security is the only authorization. Downloads do not come through
//! here: the SDK streams them and applies the statements in `schema.rs`.

use std::time::Duration;

use luma_sync::powersync::sdk::{
    error::PowerSyncError, BackendConnector, CrudEntry, PowerSyncCredentials, PowerSyncDatabase,
    UpdateType,
};
use serde_json::{Map, Value};
use sqlx::SqlitePool;

use crate::database::local::auth;

/// A connector-side failure, as an `Error` so it can be handed to
/// [`PowerSyncError::upload_error`] — the SDK's only public constructor.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UploadError(String);

fn upload_error(message: impl Into<String>) -> PowerSyncError {
    PowerSyncError::upload_error(UploadError(message.into()))
}

/// What to do about a failed request.
enum Failure {
    /// The server will never accept this entry: a policy refused it, a
    /// constraint rejected it, or the request itself is malformed. Retrying
    /// would stall every later write behind it forever.
    Rejected { status: u16, body: String },
    /// The network or the server was momentarily unavailable. The SDK retries
    /// the whole transaction with backoff.
    Transient(String),
}

pub struct Connector {
    db: PowerSyncDatabase,
    /// The app database — where rejected entries are recorded.
    pool: SqlitePool,
    /// The state database, which holds the Supabase session.
    state_pool: SqlitePool,
    http: reqwest::Client,
    postgrest_url: String,
    powersync_url: String,
    anon_key: String,
}

impl Connector {
    /// # Errors
    ///
    /// If the HTTP client cannot be built.
    pub fn new(
        db: PowerSyncDatabase,
        pool: SqlitePool,
        state_pool: SqlitePool,
    ) -> Result<Self, String> {
        Ok(Self {
            db,
            pool,
            state_pool,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|error| error.to_string())?,
            postgrest_url: crate::config::postgrest_url(),
            powersync_url: crate::config::powersync_url(),
            anon_key: crate::config::supabase_anon_key(),
        })
    }

    async fn token(&self) -> Result<String, PowerSyncError> {
        auth::get_current_access_token(&self.state_pool)
            .await
            .map_err(|error| upload_error(error.to_string()))?
            .ok_or_else(|| upload_error("no signed-in session"))
    }

    /// One PostgREST request for one CRUD entry.
    ///
    /// `PUT` upserts, so a retried insert and an insert that raced a download
    /// are the same request. `PATCH` and `DELETE` filter on `id`, which the
    /// client supplies for every synced table — including the tables whose `id`
    /// is generated locally from a natural key, because the same expression is
    /// generated in Postgres and the value agrees.
    fn request(&self, entry: &CrudEntry, token: &str) -> reqwest::RequestBuilder {
        let url = format!(
            "{}/{}",
            self.postgrest_url.trim_end_matches('/'),
            entry.table
        );
        let filter = [("id", format!("eq.{}", entry.id))];
        let builder = match entry.update_type {
            UpdateType::Put => return self.put_request(entry.table.as_ref(), &[entry], token),
            UpdateType::Patch => self
                .http
                .patch(&url)
                .query(&filter)
                .header("Prefer", "return=minimal")
                .json(&Value::Object(body(entry))),
            UpdateType::Delete => self
                .http
                .delete(&url)
                .query(&filter)
                .header("Prefer", "return=minimal"),
        };
        builder
            .header("apikey", &self.anon_key)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
    }

    /// One upsert for a run of rows in the same table.
    ///
    /// PostgREST takes an array, so a first sync of a real library is one
    /// request per few hundred rows instead of one per row — the difference
    /// between minutes and seconds. The header is the same as for a single
    /// row, because a batch *is* a single row repeated: `merge-duplicates`
    /// makes every one of them an upsert.
    fn put_request(
        &self,
        table: &str,
        entries: &[&CrudEntry],
        token: &str,
    ) -> reqwest::RequestBuilder {
        let url = format!("{}/{}", self.postgrest_url.trim_end_matches('/'), table);
        self.http
            .post(&url)
            .header("Prefer", "resolution=merge-duplicates,return=minimal")
            .json(&Value::Array(
                entries
                    .iter()
                    .map(|entry| Value::Object(body(entry)))
                    .collect(),
            ))
            .header("apikey", &self.anon_key)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
    }

    /// Send one batch of upserts, refreshing the token once on a 401.
    async fn send_put_batch(&self, table: &str, entries: &[&CrudEntry]) -> Result<(), Failure> {
        let mut token = self
            .token()
            .await
            .map_err(|e| Failure::Transient(e.to_string()))?;
        for attempt in 0..2 {
            let response = self
                .put_request(table, entries, &token)
                .send()
                .await
                .map_err(|error| Failure::Transient(error.to_string()))?;
            let status = response.status();
            if status.is_success() {
                return Ok(());
            }
            let body = response.text().await.unwrap_or_default();
            let code = status.as_u16();
            if code == 401 && attempt == 0 {
                token = self
                    .token()
                    .await
                    .map_err(|e| Failure::Transient(e.to_string()))?;
                continue;
            }
            return Err(if transient(code) {
                Failure::Transient(format!("{code}: {body}"))
            } else {
                Failure::Rejected { status: code, body }
            });
        }
        unreachable!("the retry loop returns on every path")
    }

    /// Send one entry, refreshing the token once on a 401.
    ///
    /// A 401 after a refresh is not transient — the session is gone, and the
    /// service will reconnect (or stop) when the auth watcher notices. Treating
    /// it as a rejection keeps the queue moving; the entry is recorded.
    async fn send(&self, entry: &CrudEntry) -> Result<(), Failure> {
        let mut token = self
            .token()
            .await
            .map_err(|e| Failure::Transient(e.to_string()))?;
        for attempt in 0..2 {
            let response = self
                .request(entry, &token)
                .send()
                .await
                .map_err(|error| Failure::Transient(error.to_string()))?;
            let status = response.status();
            if status.is_success() {
                return Ok(());
            }
            let body = response.text().await.unwrap_or_default();
            let code = status.as_u16();
            if code == 401 && attempt == 0 {
                token = self
                    .token()
                    .await
                    .map_err(|e| Failure::Transient(e.to_string()))?;
                continue;
            }
            return Err(if transient(code) {
                Failure::Transient(format!("{code}: {body}"))
            } else {
                Failure::Rejected { status: code, body }
            });
        }
        unreachable!("the retry loop returns on every path")
    }

    /// Record an entry the server refused, so a rejected write is recoverable
    /// rather than silently dropped when the transaction completes.
    async fn reject(&self, entry: &CrudEntry, status: u16, body: &str) {
        let op = match entry.update_type {
            UpdateType::Put => "PUT",
            UpdateType::Patch => "PATCH",
            UpdateType::Delete => "DELETE",
        };
        log::warn!(
            "[sync] {} {} {} rejected ({status}): {body}",
            op,
            entry.table,
            entry.id
        );
        let data = entry
            .data
            .as_ref()
            .map(|data| Value::Object(data.clone()).to_string());
        let result = sqlx::query(
            "INSERT INTO sync_rejections(id, table_name, row_id, op, data_json, status, body, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&entry.table)
        .bind(&entry.id)
        .bind(op)
        .bind(data)
        .bind(i64::from(status))
        .bind(body)
        .execute(&self.pool)
        .await;
        if let Err(error) = result {
            log::error!("[sync] could not record a rejected upload: {error}");
        }
    }
}

/// Whether a non-success status means "later" rather than "no".
///
/// 5xx is the server failing, not refusing. 408 and 429 are the two 4xx that
/// ask for a retry. Everything else in 4xx — a policy refusal, a constraint
/// violation, a malformed body — will say the same thing every time.
/// Join a venue by its share code, returning the venue's id.
///
/// Not a queued write, and not a local one: an ordinary client may not insert
/// a membership into a venue it cannot yet see, so `public.join_venue` is a
/// `security definer` function that writes the `venue_members` row on the
/// server. The row — and the venue, and everything in it — arrives by
/// download, which is why this is a free function rather than a method on the
/// upload connector: joining needs a session and a URL, nothing else.
///
/// # Errors
///
/// If there is no session, the code matches no venue, or the request fails.
pub async fn join_venue(state_pool: &SqlitePool, code: &str) -> Result<String, String> {
    let token = auth::get_current_access_token(state_pool)
        .await
        .map_err(|error| error.to_string())?
        .ok_or("joining a venue needs a signed-in session")?;
    let postgrest = crate::config::postgrest_url();
    let response = reqwest::Client::new()
        .post(format!(
            "{}/rpc/join_venue",
            postgrest.trim_end_matches('/')
        ))
        .header("apikey", crate::config::supabase_anon_key())
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("could not join the venue ({status}): {body}"));
    }
    // A `returns text` function answers with a bare JSON string.
    serde_json::from_str::<String>(body.trim())
        .map_err(|_| format!("unexpected join_venue response: {body}"))
}

/// Split a transaction's entries into the requests they become.
///
/// A run of consecutive upserts of one table is one request; everything else
/// is one request per entry. Consecutive is load-bearing: the queue is ordered
/// and an update or a delete between two upserts of the same row is not
/// something to reorder around.
#[must_use]
fn batches(entries: &[CrudEntry]) -> Vec<&[CrudEntry]> {
    let mut batches = Vec::new();
    let mut rest = entries;
    while let Some(first) = rest.first() {
        let run = if matches!(first.update_type, UpdateType::Put) {
            rest.iter()
                .take(PUT_BATCH)
                .take_while(|next| {
                    matches!(next.update_type, UpdateType::Put) && next.table == first.table
                })
                .count()
        } else {
            1
        };
        let (batch, tail) = rest.split_at(run);
        batches.push(batch);
        rest = tail;
    }
    batches
}

/// How many upserts go in one request.
///
/// PostgREST holds the whole body in memory and Supabase caps the request size;
/// five hundred rows of a clip or a track is well inside both, and the
/// remaining round trips are no longer what the first sync is waiting on.
const PUT_BATCH: usize = 500;

fn transient(status: u16) -> bool {
    status >= 500 || status == 408 || status == 429
}

/// The row body for a `PUT` or the changed columns for a `PATCH`.
///
/// `id` is forced in for a `PUT`: the upsert needs the conflict target present,
/// and a raw-table entry's `data` carries whatever the trigger wrote — which is
/// everything, but this makes the requirement local rather than remote.
fn body(entry: &CrudEntry) -> Map<String, Value> {
    let mut data = entry.data.clone().unwrap_or_default();
    if matches!(entry.update_type, UpdateType::Put) {
        data.insert("id".into(), Value::String(entry.id.clone()));
    }
    data
}

#[async_trait::async_trait]
impl BackendConnector for Connector {
    async fn fetch_credentials(&self) -> Result<PowerSyncCredentials, PowerSyncError> {
        Ok(PowerSyncCredentials {
            endpoint: self.powersync_url.clone(),
            token: self.token().await?,
        })
    }

    async fn upload_data(&self) -> Result<(), PowerSyncError> {
        while let Some(transaction) = self.db.next_crud_transaction().await? {
            for batch in batches(&transaction.crud) {
                let entry = &batch[0];
                let outcome = if batch.len() > 1 {
                    let entries: Vec<_> = batch.iter().collect();
                    self.send_put_batch(entry.table.as_ref(), &entries).await
                } else {
                    self.send(entry).await
                };
                match outcome {
                    Ok(()) => {}
                    Err(Failure::Rejected { status, body }) if batch.len() > 1 => {
                        // One row in the batch is bad and the server refused
                        // the lot. Retry them one at a time so the bad one is
                        // the only one that lands in `sync_rejections`.
                        log::warn!(
                            "[sync] a batch of {} {} upserts was refused ({status}): {body}",
                            batch.len(),
                            entry.table
                        );
                        for entry in batch {
                            match self.send(entry).await {
                                Ok(()) => {}
                                Err(Failure::Rejected { status, body }) => {
                                    self.reject(entry, status, &body).await;
                                }
                                Err(Failure::Transient(message)) => {
                                    return Err(upload_error(message));
                                }
                            }
                        }
                    }
                    Err(Failure::Rejected { status, body }) => {
                        self.reject(entry, status, &body).await;
                    }
                    Err(Failure::Transient(message)) => {
                        // Leave the transaction on the queue. Every entry is
                        // idempotent, so replaying the ones that already
                        // landed costs nothing but a round trip.
                        return Err(upload_error(message));
                    }
                }
            }
            transaction.complete().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connector() -> Connector {
        Connector {
            db: unreachable_database(),
            pool: SqlitePool::connect_lazy("sqlite::memory:").expect("pool"),
            state_pool: SqlitePool::connect_lazy("sqlite::memory:").expect("pool"),
            http: reqwest::Client::new(),
            postgrest_url: "https://example.test/rest/v1".into(),
            powersync_url: "https://example.test".into(),
            anon_key: "anon".into(),
        }
    }

    /// The request builder never touches the database, so the tests do not
    /// need a live one — only a value of the right type. Nothing here calls a
    /// method on it.
    fn unreachable_database() -> PowerSyncDatabase {
        use luma_sync::powersync::sdk::{
            env::PowerSyncEnvironment, schema::Schema, ConnectionPool,
        };
        PowerSyncEnvironment::powersync_auto_extension().expect("extension");
        let directory = tempfile::tempdir().expect("temp dir");
        let pool = ConnectionPool::open(&directory.path().join("unused.db")).expect("pool");
        // The directory outlives the test process, not this function: the
        // database holds its file open and never reads from it.
        std::mem::forget(directory);
        PowerSyncDatabase::new(
            PowerSyncEnvironment::custom(
                luma_sync::powersync::HttpClient::new(),
                pool,
                PowerSyncEnvironment::tokio_timer(),
            ),
            Schema::default(),
        )
    }

    fn entry(update_type: UpdateType, data: Option<Map<String, Value>>) -> CrudEntry {
        in_table("clips", "clip-1", update_type, data)
    }

    fn in_table(
        table: &str,
        id: &str,
        update_type: UpdateType,
        data: Option<Map<String, Value>>,
    ) -> CrudEntry {
        CrudEntry {
            client_id: 1,
            transaction_id: 1,
            update_type,
            table: table.into(),
            id: id.into(),
            metadata: None,
            data,
            previous_values: None,
        }
    }

    /// Consecutive upserts of one table are one request. A first sync is
    /// thousands of rows, and one request each is the difference between
    /// minutes and seconds.
    #[tokio::test]
    async fn consecutive_upserts_of_one_table_are_one_request() {
        let entries = vec![
            in_table("clips", "clip-1", UpdateType::Put, Some(row())),
            in_table("clips", "clip-2", UpdateType::Put, Some(row())),
            in_table("clips", "clip-3", UpdateType::Put, Some(row())),
            in_table("venues", "venue-1", UpdateType::Put, Some(row())),
        ];
        let batches = batches(&entries);
        assert_eq!(batches.len(), 2, "three clips and a venue are two requests");
        assert_eq!(batches[0].len(), 3);
        assert_eq!(batches[1].len(), 1);

        let request = connector()
            .put_request("clips", &batches[0].iter().collect::<Vec<_>>(), "token")
            .build()
            .expect("request");
        assert_eq!(request.method(), reqwest::Method::POST);
        let body: Value =
            serde_json::from_slice(request.body().and_then(|b| b.as_bytes()).expect("body"))
                .expect("json");
        let rows = body.as_array().expect("an array of rows");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["id"], "clip-1");
        assert_eq!(rows[2]["id"], "clip-3");
    }

    /// A PATCH or a DELETE between two upserts breaks the run: the queue is
    /// ordered, and reordering around it would change what the server sees.
    #[test]
    fn an_update_between_two_upserts_is_its_own_request() {
        let entries = vec![
            in_table("clips", "clip-1", UpdateType::Put, Some(row())),
            in_table("clips", "clip-1", UpdateType::Patch, Some(row())),
            in_table("clips", "clip-2", UpdateType::Put, Some(row())),
        ];
        let batches = batches(&entries);
        assert_eq!(batches.len(), 3);
    }

    fn row() -> Map<String, Value> {
        let mut data = Map::new();
        data.insert("uid".into(), Value::String("user-1".into()));
        data.insert("score_id".into(), Value::String("score-1".into()));
        data
    }

    #[tokio::test]
    async fn a_put_upserts_the_whole_row_with_its_id() {
        let request = connector()
            .request(&entry(UpdateType::Put, Some(row())), "token")
            .build()
            .expect("request");
        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(request.url().as_str(), "https://example.test/rest/v1/clips");
        assert_eq!(
            request.headers()["Prefer"],
            "resolution=merge-duplicates,return=minimal"
        );
        assert_eq!(request.headers()["apikey"], "anon");
        assert_eq!(request.headers()["Authorization"], "Bearer token");
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).expect("json");
        assert_eq!(body[0]["id"], "clip-1");
        assert_eq!(body[0]["uid"], "user-1");
    }

    #[tokio::test]
    async fn a_patch_filters_on_id_and_sends_only_the_changed_columns() {
        let mut data = Map::new();
        data.insert("duration".into(), Value::from(4.0));
        let request = connector()
            .request(&entry(UpdateType::Patch, Some(data)), "token")
            .build()
            .expect("request");
        assert_eq!(request.method(), reqwest::Method::PATCH);
        assert_eq!(
            request.url().as_str(),
            "https://example.test/rest/v1/clips?id=eq.clip-1"
        );
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).expect("json");
        assert_eq!(body, serde_json::json!({ "duration": 4.0 }));
    }

    #[tokio::test]
    async fn a_delete_filters_on_id_and_carries_no_body() {
        let request = connector()
            .request(&entry(UpdateType::Delete, None), "token")
            .build()
            .expect("request");
        assert_eq!(request.method(), reqwest::Method::DELETE);
        assert_eq!(
            request.url().as_str(),
            "https://example.test/rest/v1/clips?id=eq.clip-1"
        );
        assert!(request
            .body()
            .is_none_or(|body| body.as_bytes() == Some(b"")));
    }

    #[test]
    fn only_server_failures_and_backpressure_are_retried() {
        assert!(transient(500) && transient(503) && transient(408) && transient(429));
        assert!(!transient(400) && !transient(401) && !transient(403));
        assert!(!transient(409) && !transient(422) && !transient(404));
    }
}
