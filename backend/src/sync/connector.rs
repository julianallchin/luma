//! Uploads: the PowerSync CRUD queue drained into Supabase PostgREST.
//!
//! A local write becomes a row in `ps_crud` (see `triggers.rs`) and this
//! connector replays it against PostgREST as the signed-in user, so Postgres
//! row-level security is the only authorization. Downloads do not come through
//! here: the SDK streams them and applies the statements in `schema.rs`.
//!
//! An attributed entry carries who wrote it and what it belongs to as
//! metadata (see `triggers.rs`), and the request carries each part as one of
//! [`ATTRIBUTION_HEADERS`] for the server's history trigger. A request without
//! them is the signed-in person's. Every request names its change group: the
//! entry's own, or one made for the local transaction it was written in, so
//! what was written together is recorded together.
//!
//! The server never deletes a source row (docs/design/data-layer.md), so a
//! queued delete of one uploads as an update that sets `deleted_at`, and every
//! upsert of one sends `deleted_at: null`: inserting a row under the id of a
//! flagged one is how it comes back. A derived or scratch row is deleted for
//! real.
//!
//! A refusal is never silent. An upsert the policies refuse is a 403, but an
//! update or a delete they refuse is filtered: PostgREST answers 204 and
//! changes no row. So those ask for the count of rows they changed, and a
//! count of zero is a refusal like any other — recorded in `sync_rejections`,
//! taken off the queue, and replaced by the server's version of the row on the
//! next download.
//!
//! Every request says which data format this build reads, in
//! [`FORMAT_HEADER`]. A server whose data has moved past it refuses the write
//! with a 426, and before the stream opens the connector asks the server's
//! format itself: a build behind it does not connect, because it would read
//! the rows wrongly. Either way [`Connector`]'s `outdated` flag goes up, which
//! the status reports as "update required". See
//! `supabase/migrations/20261111120000_client_format.sql`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use luma_sync::powersync::sdk::{
    error::PowerSyncError, BackendConnector, CrudEntry, PowerSyncCredentials, PowerSyncDatabase,
    UpdateType,
};
use serde_json::{Map, Value};
use sqlx::SqlitePool;

use crate::database::local::auth;
use crate::database::local::database::PROJECTION_VERSION;

use super::schema::Kind;
use super::supabase::FORMAT_HEADER;

/// The header the history trigger reads the actor from.
const ACTOR_HEADER: &str = "x-luma-actor";

/// The header naming the changes a request's rows were made together with.
const GROUP_HEADER: &str = "x-luma-group";

/// The parts of an entry's attribution that only the entry can supply, and
/// the header each travels in. The fourth part, `group`, always travels: see
/// [`attributed`].
const ATTRIBUTION_HEADERS: [(&str, &str); 3] = [
    ("actor", ACTOR_HEADER),
    ("thread", "x-luma-thread"),
    ("message", "x-luma-message"),
];

/// The column a source row's delete sets, and an upsert of one clears.
const FLAG: &str = "deleted_at";

/// Whether the server keeps `table`'s rows for good, flagging a deleted one
/// instead of removing it.
fn is_source(table: &str) -> bool {
    super::schema::table(table).is_some_and(|table| table.kind == Kind::Source)
}

/// A connector-side failure, as an `Error` so it can be handed to
/// [`PowerSyncError::upload_error`] — the SDK's only public constructor.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UploadError(String);

fn upload_error(message: impl Into<String>) -> PowerSyncError {
    PowerSyncError::upload_error(UploadError(message.into()))
}

/// What a refusal the server answered with success records as its body.
const NO_ROW_CHANGED: &str = "the server changed no row";

/// The status the server refuses a build behind its data format with.
const UPDATE_REQUIRED: u16 = 426;

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
    credentials: Credentials,
    http: reqwest::Client,
    postgrest_url: String,
    powersync_url: String,
    anon_key: String,
    /// The data format this build reads.
    format: i64,
    /// Raised when the server's data is past `format`. Shared with the
    /// service, which reports it and stops connecting.
    outdated: Arc<AtomicBool>,
    /// The local transaction being uploaded and the change group made for
    /// it. Kept so a transaction retried after a failure part-way through
    /// sends the rest of its entries under the group the first ones went in.
    group: Mutex<Option<(Option<i64>, String)>>,
}

/// Where the access token comes from.
enum Credentials {
    /// The state database, which holds the Supabase session.
    Session(SqlitePool),
    /// A token minted for a test against the container stack, which has no
    /// Supabase auth to sign in through.
    #[cfg(test)]
    Fixed(String),
}

impl Connector {
    /// # Errors
    ///
    /// If the HTTP client cannot be built.
    pub fn new(
        db: PowerSyncDatabase,
        pool: SqlitePool,
        state_pool: SqlitePool,
        outdated: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        Ok(Self {
            db,
            pool,
            credentials: Credentials::Session(state_pool),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|error| error.to_string())?,
            postgrest_url: crate::config::postgrest_url(),
            powersync_url: crate::config::powersync_url(),
            anon_key: crate::config::SUPABASE_ANON_KEY.to_owned(),
            format: PROJECTION_VERSION,
            outdated,
            group: Mutex::default(),
        })
    }

    /// The production connector signed in with `token` against the given
    /// PostgREST and PowerSync — the container stack's.
    #[cfg(test)]
    pub(super) fn with_token(
        db: PowerSyncDatabase,
        pool: SqlitePool,
        token: String,
        postgrest_url: String,
        powersync_url: String,
    ) -> Self {
        Self {
            db,
            pool,
            anon_key: token.clone(),
            credentials: Credentials::Fixed(token),
            http: reqwest::Client::new(),
            postgrest_url,
            powersync_url,
            format: PROJECTION_VERSION,
            outdated: Arc::default(),
            group: Mutex::default(),
        }
    }

    /// The same connector as a build that reads data `format`, raising
    /// `outdated` when the server is past it.
    #[cfg(test)]
    pub(super) fn reading(mut self, format: i64, outdated: Arc<AtomicBool>) -> Self {
        self.format = format;
        self.outdated = outdated;
        self
    }

    /// Whether the server's data is past what this build reads; the flag
    /// goes up if it is.
    async fn behind(&self, token: &str) -> Option<i64> {
        let server = server_format(&self.http, &self.postgrest_url, &self.anon_key, token).await?;
        (server > self.format).then(|| {
            self.outdated.store(true, Ordering::Release);
            server
        })
    }

    async fn token(&self) -> Result<String, PowerSyncError> {
        match &self.credentials {
            Credentials::Session(state_pool) => auth::get_current_access_token(state_pool)
                .await
                .map_err(|error| upload_error(error.to_string()))?
                .ok_or_else(|| upload_error("no signed-in session")),
            #[cfg(test)]
            Credentials::Fixed(token) => Ok(token.clone()),
        }
    }

    fn url(&self, table: &str) -> String {
        format!("{}/{table}", self.postgrest_url.trim_end_matches('/'))
    }

    /// The change group for the entries of local transaction `transaction`
    /// that name none: the same one each time the transaction is tried.
    fn group_of(&self, transaction: Option<i64>) -> String {
        let mut current = self
            .group
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*current {
            Some((held, group)) if *held == transaction => group.clone(),
            _ => {
                let group = uuid::Uuid::new_v4().to_string();
                *current = Some((transaction, group.clone()));
                group
            }
        }
    }

    /// One PostgREST request for one CRUD entry.
    ///
    /// `PUT` upserts, so a retried insert and an insert that raced a download
    /// are the same request. `PATCH` and `DELETE` filter on `id`, which the
    /// client supplies for every synced table — including a derived table's,
    /// whose `id` is built locally from its natural key, because Postgres
    /// builds it with the same expression and the value agrees.
    ///
    /// A `DELETE` of a source row is sent as an update that flags it; the
    /// server stamps who. `PATCH` and `DELETE`, flag or not, ask for the count
    /// of rows they changed: a policy refuses them by filtering the row out,
    /// not with an error.
    fn request(&self, entry: &CrudEntry, token: &str, group: &str) -> reqwest::RequestBuilder {
        let url = self.url(&entry.table);
        let filter = [("id", format!("eq.{}", entry.id))];
        let builder = match entry.update_type {
            UpdateType::Put => {
                return self.put_request(entry.table.as_ref(), &[entry], token, group)
            }
            UpdateType::Patch => self
                .http
                .patch(&url)
                .query(&filter)
                .header("Prefer", COUNTED)
                .json(&Value::Object(body(entry))),
            UpdateType::Delete if is_source(&entry.table) => self
                .http
                .patch(&url)
                .query(&filter)
                .header("Prefer", COUNTED)
                .json(&serde_json::json!({ FLAG: now() })),
            UpdateType::Delete => self
                .http
                .delete(&url)
                .query(&filter)
                .header("Prefer", COUNTED),
        };
        attributed(builder, entry, group)
            .header("apikey", &self.anon_key)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header(FORMAT_HEADER, self.format)
    }

    /// One upsert for a run of rows in the same table.
    ///
    /// PostgREST takes an array, so a first sync of a real library is one
    /// request per few hundred rows instead of one per row — the difference
    /// between minutes and seconds. The header is the same as for a single
    /// row, because a batch *is* a single row repeated: `merge-duplicates`
    /// makes every one of them an upsert. Every entry in a batch has the same
    /// attribution, so the first one speaks for all.
    fn put_request(
        &self,
        table: &str,
        entries: &[&CrudEntry],
        token: &str,
        group: &str,
    ) -> reqwest::RequestBuilder {
        let url = self.url(table);
        let builder = self
            .http
            .post(&url)
            .header("Prefer", "resolution=merge-duplicates,return=minimal");
        let builder = match entries.first() {
            Some(first) => attributed(builder, first, group),
            None => builder,
        };
        builder
            .json(&Value::Array(
                entries
                    .iter()
                    .map(|entry| Value::Object(body(entry)))
                    .collect(),
            ))
            .header("apikey", &self.anon_key)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header(FORMAT_HEADER, self.format)
    }

    /// Send one batch of upserts, refreshing the token once on a 401.
    async fn send_put_batch(
        &self,
        table: &str,
        entries: &[&CrudEntry],
        group: &str,
    ) -> Result<(), Failure> {
        let mut token = self
            .token()
            .await
            .map_err(|e| Failure::Transient(e.to_string()))?;
        for attempt in 0..2 {
            let response = self
                .put_request(table, entries, &token, group)
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
    async fn send(&self, entry: &CrudEntry, group: &str) -> Result<(), Failure> {
        let mut token = self
            .token()
            .await
            .map_err(|e| Failure::Transient(e.to_string()))?;
        for attempt in 0..2 {
            let response = self
                .request(entry, &token, group)
                .send()
                .await
                .map_err(|error| Failure::Transient(error.to_string()))?;
            let status = response.status();
            if status.is_success() {
                return self.changed(entry, &response, &token).await;
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

    /// Whether a successful `PATCH` or `DELETE` changed its row.
    ///
    /// Zero rows is a refusal, with one exception: a delete of a row that is
    /// not there for this user any more — its parent was deleted first and
    /// took it along, or it was never theirs to see — already has the outcome
    /// it asked for. That holds for a delete sent as a flag too. Flagging a
    /// row that is already flagged counts as a change, so it never gets here.
    async fn changed(
        &self,
        entry: &CrudEntry,
        response: &reqwest::Response,
        token: &str,
    ) -> Result<(), Failure> {
        if matches!(entry.update_type, UpdateType::Put) || changed_rows(response) != Some(0) {
            return Ok(());
        }
        let refused = Failure::Rejected {
            status: response.status().as_u16(),
            body: NO_ROW_CHANGED.into(),
        };
        if matches!(entry.update_type, UpdateType::Patch) {
            return Err(refused);
        }
        let visible = self
            .http
            .get(self.url(&entry.table))
            .query(&[("id", format!("eq.{}", entry.id)), ("select", "id".into())])
            .header("apikey", &self.anon_key)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|error| Failure::Transient(error.to_string()))?;
        if !visible.status().is_success() {
            let code = visible.status().as_u16();
            return Err(Failure::Transient(format!(
                "{code}: {}",
                visible.text().await.unwrap_or_default()
            )));
        }
        let rows: Vec<Value> = visible
            .json()
            .await
            .map_err(|error| Failure::Transient(error.to_string()))?;
        if rows.is_empty() {
            Ok(())
        } else {
            Err(refused)
        }
    }

    /// Record an entry the server refused, so a rejected write is recoverable
    /// rather than silently dropped when the transaction completes.
    ///
    /// This is the one place a refusal is known. The app has no telemetry
    /// yet; when it does, a refusal event belongs here, beside the log line.
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
        if status == UPDATE_REQUIRED {
            self.outdated.store(true, Ordering::Release);
        }
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

/// The format of the server's data, or `None` when it would not say: no
/// network, or a server from before it had one. The write check is the
/// server's own, so not knowing is not a reason to stay disconnected.
pub(super) async fn server_format(
    http: &reqwest::Client,
    postgrest_url: &str,
    anon_key: &str,
    token: &str,
) -> Option<i64> {
    let response = http
        .post(format!(
            "{}/rpc/data_format",
            postgrest_url.trim_end_matches('/')
        ))
        .header("apikey", anon_key)
        .header("Authorization", format!("Bearer {token}"))
        .json(&Value::Object(Map::new()))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

/// Put the entry's attribution on its request: one header per part it has,
/// and its change group always.
///
/// The metadata is the object the upload triggers wrote. Anything else is an
/// entry nobody attributed, which is the signed-in person's. An entry that
/// names no group belongs to `group`, the one made for its local transaction:
/// the server would otherwise group by request, and one local transaction is
/// many requests.
fn attributed(
    mut builder: reqwest::RequestBuilder,
    entry: &CrudEntry,
    group: &str,
) -> reqwest::RequestBuilder {
    let attribution = match entry.metadata.as_deref().map(serde_json::from_str::<Value>) {
        Some(Ok(Value::Object(attribution))) => attribution,
        _ => Map::new(),
    };
    for (part, header) in ATTRIBUTION_HEADERS {
        if let Some(value) = attribution.get(part).and_then(Value::as_str) {
            builder = builder.header(header, value);
        }
    }
    builder.header(
        GROUP_HEADER,
        attribution
            .get("group")
            .and_then(Value::as_str)
            .unwrap_or(group),
    )
}

/// The moment a delete is uploaded, as the server's `timestamptz` reads it.
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Split a transaction's entries into the requests they become.
///
/// A run of consecutive upserts of one table with one attribution is one
/// request; everything else is one request per entry. Consecutive is
/// load-bearing: the queue is ordered and an update or a delete between two
/// upserts of the same row is not something to reorder around. One
/// attribution, because it travels per request.
#[must_use]
fn batches(entries: &[CrudEntry]) -> Vec<&[CrudEntry]> {
    let mut batches = Vec::new();
    let mut rest = entries;
    while let Some(first) = rest.first() {
        let run = if matches!(first.update_type, UpdateType::Put) {
            rest.iter()
                .take(PUT_BATCH)
                .take_while(|next| {
                    matches!(next.update_type, UpdateType::Put)
                        && next.table == first.table
                        && next.metadata == first.metadata
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

/// The `Prefer` header of an update or a delete: no body back, but the count
/// of rows changed in `Content-Range`.
const COUNTED: &str = "return=minimal,count=exact";

/// The total in a counted response's `Content-Range` (`0-0/1`, `*/0`), if it
/// has one.
fn changed_rows(response: &reqwest::Response) -> Option<u64> {
    content_range_total(response.headers().get("Content-Range")?.to_str().ok()?)
}

fn content_range_total(range: &str) -> Option<u64> {
    range.rsplit_once('/')?.1.trim().parse().ok()
}

fn transient(status: u16) -> bool {
    status >= 500 || status == 408 || status == 429
}

/// The row body for a `PUT` or the changed columns for a `PATCH`.
///
/// `id` is forced in for a `PUT`: the upsert needs the conflict target present,
/// and a raw-table entry's `data` carries whatever the trigger wrote — which is
/// everything, but this makes the requirement local rather than remote.
///
/// A `PUT` of a source row says it is live. The local row has no flag to
/// send, and an upsert that left the column out would land on a flagged row
/// and leave it flagged.
fn body(entry: &CrudEntry) -> Map<String, Value> {
    let mut data = entry.data.clone().unwrap_or_default();
    if matches!(entry.update_type, UpdateType::Put) {
        data.insert("id".into(), Value::String(entry.id.clone()));
        if is_source(&entry.table) {
            data.insert(FLAG.into(), Value::Null);
        }
    }
    data
}

#[async_trait::async_trait]
impl BackendConnector for Connector {
    async fn fetch_credentials(&self) -> Result<PowerSyncCredentials, PowerSyncError> {
        let token = self.token().await?;
        // Before the stream opens: a build behind the server's data would
        // apply rows it reads wrongly.
        if let Some(server) = self.behind(&token).await {
            return Err(upload_error(format!(
                "this build reads data format {} and the server holds format {server}",
                self.format
            )));
        }
        Ok(PowerSyncCredentials {
            endpoint: self.powersync_url.clone(),
            token,
        })
    }

    async fn upload_data(&self) -> Result<(), PowerSyncError> {
        while let Some(transaction) = self.db.next_crud_transaction().await? {
            let group = self.group_of(transaction.id);
            for batch in batches(&transaction.crud) {
                let entry = &batch[0];
                let outcome = if batch.len() > 1 {
                    let entries: Vec<_> = batch.iter().collect();
                    self.send_put_batch(entry.table.as_ref(), &entries, &group)
                        .await
                } else {
                    self.send(entry, &group).await
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
                            match self.send(entry, &group).await {
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
            credentials: Credentials::Session(
                SqlitePool::connect_lazy("sqlite::memory:").expect("pool"),
            ),
            http: reqwest::Client::new(),
            postgrest_url: "https://example.test/rest/v1".into(),
            powersync_url: "https://example.test".into(),
            anon_key: "anon".into(),
            format: PROJECTION_VERSION,
            outdated: Arc::default(),
            group: Mutex::default(),
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

    /// Rows two actors wrote are two requests, each naming its own actor: the
    /// server's history reads the actor off the request.
    #[tokio::test]
    async fn upserts_by_different_actors_are_separate_requests() {
        let mut model = in_table("clips", "clip-2", UpdateType::Put, Some(row()));
        model.metadata = Some(by_the_model());
        let entries = vec![
            in_table("clips", "clip-1", UpdateType::Put, Some(row())),
            model,
        ];
        let batches = batches(&entries);
        assert_eq!(batches.len(), 2);

        let connector = connector();
        let person = connector
            .put_request(
                "clips",
                &batches[0].iter().collect::<Vec<_>>(),
                "token",
                GROUP,
            )
            .build()
            .expect("request");
        assert!(person.headers().get(ACTOR_HEADER).is_none());
        let agent = connector
            .put_request(
                "clips",
                &batches[1].iter().collect::<Vec<_>>(),
                "token",
                GROUP,
            )
            .build()
            .expect("request");
        assert_eq!(agent.headers()[ACTOR_HEADER], "claude-opus-5-5");
    }

    /// The group a test's local transaction was given.
    const GROUP: &str = "transaction-group";

    /// One local transaction is one change group: an entry that names no
    /// group travels under its transaction's, whatever request it lands in,
    /// and a transaction tried again keeps the group it had. An entry that
    /// names its own group keeps that.
    #[tokio::test]
    async fn an_entry_without_a_group_takes_its_transaction_s() {
        let connector = connector();
        let first = connector.group_of(Some(7));
        assert_eq!(connector.group_of(Some(7)), first, "a retry is one group");
        assert_ne!(connector.group_of(Some(8)), first, "the next is another");

        let mut model = entry(UpdateType::Patch, Some(row()));
        model.metadata = Some(by_the_model());
        for (entry, group) in [
            (entry(UpdateType::Put, Some(row())), GROUP),
            (entry(UpdateType::Patch, Some(row())), GROUP),
            (entry(UpdateType::Delete, None), GROUP),
            (model, "group-1"),
        ] {
            let request = connector
                .request(&entry, "token", GROUP)
                .build()
                .expect("request");
            assert_eq!(request.headers()[GROUP_HEADER], group);
        }
    }

    /// The server keeps a source row for good, so deleting one is an update
    /// that flags it, counted like any update so a refusal shows.
    #[tokio::test]
    async fn a_delete_of_a_source_row_is_sent_as_a_flag() {
        let request = connector()
            .request(&entry(UpdateType::Delete, None), "token", GROUP)
            .build()
            .expect("request");
        assert_eq!(request.method(), reqwest::Method::PATCH);
        assert_eq!(
            request.url().as_str(),
            "https://example.test/rest/v1/clips?id=eq.clip-1"
        );
        assert_eq!(request.headers()["Prefer"], COUNTED);
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).expect("json");
        let flag = body.as_object().expect("an object");
        assert_eq!(flag.len(), 1, "the flag is the whole change: {body}");
        let at = flag[FLAG].as_str().expect("a timestamp");
        let at = chrono::DateTime::parse_from_rfc3339(at).expect("ISO 8601");
        assert_eq!(at.offset().local_minus_utc(), 0, "in UTC");
    }

    /// An insert under a flagged row's id is how the row comes back, so every
    /// upsert of a source row says it is live. A derived or a scratch row has
    /// no flag to clear, and the server has no such column to take.
    #[tokio::test]
    async fn an_upsert_of_a_source_row_clears_its_flag() {
        let connector = connector();
        let body = |table: &str| {
            let request = connector
                .request(
                    &in_table(table, "row-1", UpdateType::Put, Some(row())),
                    "token",
                    GROUP,
                )
                .build()
                .expect("request");
            serde_json::from_slice::<Value>(request.body().unwrap().as_bytes().unwrap())
                .expect("json")[0]
                .clone()
        };
        let clip = body("clips");
        assert_eq!(clip.get(FLAG), Some(&Value::Null));
        for table in ["track_stems", "drafts"] {
            assert!(body(table).get(FLAG).is_none(), "{table} has no flag");
        }
    }

    /// A delete has no row left to say who removed it, so the request does.
    #[tokio::test]
    async fn an_attributed_delete_names_its_actor() {
        let mut delete = entry(UpdateType::Delete, None);
        delete.metadata = Some(by_the_model());
        let request = connector()
            .request(&delete, "token", GROUP)
            .build()
            .expect("request");
        assert_eq!(request.headers()[ACTOR_HEADER], "claude-opus-5-5");
        assert_eq!(request.headers()["x-luma-thread"], "thread-1");
        assert_eq!(request.headers()[GROUP_HEADER], "group-1");
        assert!(
            request.headers().get("x-luma-message").is_none(),
            "a part the write does not have sends no header"
        );
    }

    /// An entry's metadata as the upload triggers write it for a model's turn.
    fn by_the_model() -> String {
        serde_json::json!({
            "actor": "claude-opus-5-5",
            "thread": "thread-1",
            "message": null,
            "group": "group-1",
        })
        .to_string()
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
            .put_request(
                "clips",
                &batches[0].iter().collect::<Vec<_>>(),
                "token",
                GROUP,
            )
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
            .request(&entry(UpdateType::Put, Some(row())), "token", GROUP)
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
            .request(&entry(UpdateType::Patch, Some(data)), "token", GROUP)
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

    /// A derived row is a cache and a draft is scratch: both are deleted for
    /// real.
    #[tokio::test]
    async fn a_delete_of_a_derived_or_scratch_row_filters_on_id_and_carries_no_body() {
        for table in ["track_stems", "drafts"] {
            a_real_delete(table);
        }
    }

    fn a_real_delete(table: &str) {
        let request = connector()
            .request(
                &in_table(table, "row-1", UpdateType::Delete, None),
                "token",
                GROUP,
            )
            .build()
            .expect("request");
        assert_eq!(request.method(), reqwest::Method::DELETE);
        assert_eq!(
            request.url().as_str(),
            format!("https://example.test/rest/v1/{table}?id=eq.row-1")
        );
        assert!(request
            .body()
            .is_none_or(|body| body.as_bytes() == Some(b"")));
    }

    /// An update or a delete a policy filters out answers success, so the
    /// only sign of the refusal is the count it asks for.
    #[tokio::test]
    async fn an_update_and_a_delete_ask_for_the_rows_they_changed() {
        let connector = connector();
        for (table, update_type) in [
            ("clips", UpdateType::Patch),
            ("clips", UpdateType::Delete),
            ("drafts", UpdateType::Delete),
        ] {
            let request = connector
                .request(
                    &in_table(table, "row-1", update_type, Some(row())),
                    "token",
                    GROUP,
                )
                .build()
                .expect("request");
            assert_eq!(request.headers()["Prefer"], COUNTED);
        }
    }

    #[test]
    fn the_count_is_the_total_of_the_content_range() {
        assert_eq!(content_range_total("*/0"), Some(0));
        assert_eq!(content_range_total("0-0/1"), Some(1));
        assert_eq!(content_range_total("*/*"), None);
    }

    #[test]
    fn only_server_failures_and_backpressure_are_retried() {
        assert!(transient(500) && transient(503) && transient(408) && transient(429));
        assert!(!transient(400) && !transient(401) && !transient(403));
        assert!(!transient(409) && !transient(422) && !transient(404));
    }
}
