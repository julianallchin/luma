use luma_sync::powersync::Connections;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use std::path::Path;

/// The format of the synced tables' local copy.
///
/// Bump it when a client can no longer read rows an older one downloaded. A
/// library whose `user_version` differs clears its synced tables at startup
/// and PowerSync downloads them again — so no code ever converts a local row.
/// Edits still waiting to upload were written in the old format and cannot be
/// sent, so they are listed as not saved first: see
/// [`reset_stale_projection`].
pub const PROJECTION_VERSION: i64 = 3;

/// `SqlitePool` is itself a shared handle, so cloning `Db` hands out another
/// reference to the same pool, never a second pool.
#[derive(Clone)]
pub struct Db(pub SqlitePool);

/// Create the directory and run every migration.
///
/// Migrations run on their own connection with foreign keys off — several of
/// them rebuild a table and would trip over their own child rows otherwise.
/// Returns where the database is, and that connection for the caller to close
/// once its own pool is open: the last connection to a file checkpoints it on
/// close, which on a large library took ~100 ms of every launch.
async fn migrate_app_db_at(app_dir: &Path) -> Result<(std::path::PathBuf, SqlitePool), String> {
    std::fs::create_dir_all(app_dir).map_err(|error| {
        format!(
            "Failed to create app config dir {}: {error}",
            app_dir.display()
        )
    })?;
    let db_path = crate::storage::StorageRoot::from_path(app_dir.to_path_buf()).luma_db_path();
    let migrate_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .map_err(|error| {
            format!(
                "Failed to connect to database at {}: {error}",
                db_path.display()
            )
        })?;

    let migrator = sqlx::migrate!("./migrations");
    adopt_baseline(&migrate_pool, &migrator, &db_path).await?;
    migrator
        .run(&migrate_pool)
        .await
        .map_err(|error| format!("Failed to run app migrations: {error}"))?;
    Ok((db_path, migrate_pool))
}

/// The last of the migrations `20261024000000_baseline` replaced.
const LAST_SQUASHED: i64 = 20_261_023_120_000;

/// Give a library migrated through every squashed migration the baseline's
/// history, and refuse one that stopped short of them.
///
/// Such a library already has the baseline's schema, so only the
/// `_sqlx_migrations` rows change. A fresh file, or one already on the
/// baseline, is left alone. Delete this once every nightly install has
/// launched a build that carries it.
async fn adopt_baseline(
    pool: &SqlitePool,
    migrator: &sqlx::migrate::Migrator,
    db_path: &Path,
) -> Result<(), String> {
    let baseline = migrator
        .iter()
        .next()
        .expect("the baseline is the first migration");
    let history: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("Failed to read migration history: {error}"))?;
    let versions: Vec<i64> = match history {
        Some(_) => sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
            .fetch_all(pool)
            .await
            .map_err(|error| format!("Failed to read migration history: {error}"))?,
        None => Vec::new(),
    };
    if versions.is_empty() || versions.contains(&baseline.version) {
        return Ok(());
    }
    if !versions.contains(&LAST_SQUASHED) {
        return Err(format!(
            "The library at {} was last opened by a Luma version from before 23 October 2026, \
             which this version cannot upgrade. Quit Luma, move that file (with any \
             luma.db-wal and luma.db-shm beside it) somewhere else, and open Luma again \
             to start a new library. What you synced downloads again when you sign in; \
             the moved file keeps the rest.",
            db_path.display()
        ));
    }
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| format!("Failed to adopt the baseline migration: {error}"))?;
    sqlx::query("DELETE FROM _sqlx_migrations")
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to adopt the baseline migration: {error}"))?;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
         VALUES (?1, ?2, TRUE, ?3, 0)",
    )
    .bind(baseline.version)
    .bind(&*baseline.description)
    .bind(&*baseline.checksum)
    .execute(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to adopt the baseline migration: {error}"))?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to adopt the baseline migration: {error}"))
}

/// Migrate and open the app database without the sync SDK.
///
/// The same migrations, on an ordinary SQLx pool. What it does not get is the
/// SDK's connection pair, and so no upload queue: see
/// [`crate::sync::triggers::install`]. A test that wants one makes its own; a
/// host that wants to sync calls [`open_app_db_at`].
///
/// # Errors
///
/// If the directory cannot be created, a migration fails, or the file cannot
/// be opened.
pub async fn init_app_db_at(app_dir: &Path) -> Result<Db, String> {
    let (db_path, migrate_pool) = migrate_app_db_at(app_dir).await?;
    let pool = SqlitePoolOptions::new()
        .max_connections(16)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .map_err(|error| {
            format!(
                "Failed to connect to database at {}: {error}",
                db_path.display()
            )
        })?;
    migrate_pool.close().await;
    Ok(Db(pool))
}

/// Migrate and open the app database on the connection pair PowerSync needs.
///
/// One SQLite file, two pools: the SQLx pool every writer uses, and the SDK's
/// own. A host that syncs hands the returned [`Connections`] to
/// `Connections::start` and then to `sync::service::Service`; the pool inside
/// it *is* `Db`'s pool, so there is never a second view of the database.
///
/// # Errors
///
/// If the directory cannot be created, a migration fails, or the file cannot
/// be opened.
pub async fn open_app_db_at(app_dir: &Path) -> Result<(Db, Connections), String> {
    let (db_path, migrate_pool) = migrate_app_db_at(app_dir).await?;

    // Now open the real pool WITH foreign_keys enabled.
    //
    // Every pooled connection is a writer, so every one carries the PowerSync
    // upload queue. The triggers are TEMP, so they belong to the connection
    // and never to the file: the sync SDK holds its own pool and deliberately
    // has none, which is what keeps a downloaded row out of the upload queue.
    let connections = Connections::open_with(&db_path, 16, |connection| {
        Box::pin(async move {
            crate::sync::triggers::install(connection)
                .await
                .map_err(|error| sqlx::Error::Configuration(error.into()))
        })
    })
    .await
    .map_err(|error| {
        format!(
            "Failed to connect to database at {}: {error}",
            db_path.display()
        )
    })?;
    migrate_pool.close().await;
    create_sync_tables(connections.sql()).await?;
    reset_stale_projection(connections.sql()).await?;

    Ok((Db(connections.sql().clone()), connections))
}

/// Create PowerSync's own tables in a library it has not opened yet.
///
/// The SDK does this when it starts, which is after the first write here: the
/// projection reset below deletes from the synced tables, and a statement
/// that can fire an upload trigger opens the upload queue whether or not a
/// row is deleted. Without the queue's tables the extension reports the
/// failure in memory SQLite did not allocate, and the process aborts. A
/// library that has them is left as it is.
async fn create_sync_tables(pool: &SqlitePool) -> Result<(), String> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| format!("Failed to begin creating the sync tables: {error}"))?;
    sqlx::query("SELECT powersync_init()")
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to create the sync tables: {error}"))?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit the sync tables: {error}"))
}

/// Empty every synced table and PowerSync's download state, so the library
/// is downloaded again from nothing. The clear is marked local, so nothing it
/// removes reaches the server; the caller turns upload capture back on
/// ([`crate::sync::triggers::uploading`]) before it commits.
///
/// Clearing PowerSync's state drops its upload queue with it. What happens
/// to a change still waiting there is the caller's to decide first.
async fn clear_synced_tables(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), String> {
    let synced_before = synced_before(transaction).await?;
    crate::sync::triggers::local_only(transaction).await?;
    // Every synced table goes, so a reference between two of them is gone by
    // the commit whichever order they are cleared in. Admission guards a write
    // made *as* someone; an eviction is nobody's, so it is disarmed for the
    // clear and put back as it was.
    let armed: Option<i64> =
        sqlx::query_scalar("SELECT armed FROM auth_write_admission WHERE singleton = 1")
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| format!("Failed to read write admission: {error}"))?;
    sqlx::raw_sql(
        "PRAGMA defer_foreign_keys = ON;
         UPDATE auth_write_admission SET armed = 0 WHERE singleton = 1;",
    )
    .execute(&mut **transaction)
    .await
    .map_err(|error| format!("Failed to prepare the projection reset: {error}"))?;
    for table in crate::sync::schema::SYNCED_TABLES.iter().rev() {
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {}", table.name)))
            .execute(&mut **transaction)
            .await
            .map_err(|error| format!("Failed to clear {}: {error}", table.name))?;
    }
    sqlx::query("UPDATE auth_write_admission SET armed = ? WHERE singleton = 1")
        .bind(armed.unwrap_or(0))
        .execute(&mut **transaction)
        .await
        .map_err(|error| format!("Failed to restore write admission: {error}"))?;
    if synced_before {
        sqlx::query("SELECT powersync_clear(0)")
            .execute(&mut **transaction)
            .await
            .map_err(|error| format!("Failed to reset the sync state: {error}"))?;
    }
    Ok(())
}

/// Whether this library has PowerSync's tables. One opened by a host that
/// does not sync ([`init_app_db_at`]) has none.
async fn synced_before(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<bool, String> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ps_buckets')",
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| format!("Failed to inspect the sync state: {error}"))
}

/// The changes waiting to upload, oldest first: each one's table, row id,
/// operation and the columns it carries, as the queue holds them.
async fn waiting_to_upload(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<Vec<(String, String, String, Option<String>)>, String> {
    if !synced_before(transaction).await? {
        return Ok(Vec::new());
    }
    sqlx::query_as(
        "SELECT json_extract(data, '$.type'), json_extract(data, '$.id'),
                json_extract(data, '$.op'), json_extract(data, '$.data')
         FROM ps_crud ORDER BY id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| format!("Failed to read the changes waiting to sync: {error}"))
}

/// What a change that was never sent records as its status: no server
/// answered it.
const NEVER_SENT: i64 = 0;

/// What a change set aside at a format change records as its body.
const PREDATES_UPDATE: &str =
    "this change was made before an update changed how Luma stores data, and was not sent";

/// List every change waiting to upload as not saved, the way the upload
/// connector lists one the server refused. They were written in the format
/// the library is leaving and the server no longer takes it, so the queue
/// they sit in is about to be dropped with the rest of the old copy.
async fn set_aside_waiting_uploads(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<usize, String> {
    let waiting = waiting_to_upload(transaction).await?;
    for (table, row_id, op, data) in &waiting {
        sqlx::query(
            "INSERT INTO sync_rejections(id, table_name, row_id, op, data_json, status, body, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(table)
        .bind(row_id)
        .bind(op)
        .bind(data)
        .bind(NEVER_SENT)
        .bind(PREDATES_UPDATE)
        .execute(&mut **transaction)
        .await
        .map_err(|error| format!("Failed to record a change that was not sent: {error}"))?;
    }
    Ok(waiting.len())
}

/// Clear the library when it is another account's, so `principal` downloads
/// their own from nothing. Runs between disconnecting one account's sync and
/// connecting the next.
///
/// PowerSync's state is one account's: its write checkpoints count that
/// account's uploads, and a stream opened as someone else can never reach
/// them, so its downloads are fetched and never applied. A library is
/// another account's when it holds a venue or a track `principal` has no
/// standing on. Rows nobody owns are a library from before accounts, which
/// exists nowhere else: one of those postpones the clear, as a change still
/// waiting to upload does.
///
/// # Errors
///
/// If the library cannot be read, or the clear fails.
pub(crate) async fn evict_other_account(
    pool: &SqlitePool,
    principal: &str,
) -> Result<bool, String> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin the account check: {error}"))?;
    // The views below answer for whoever is admitted, so they are only
    // evidence once that is `principal`.
    let admitted: Option<String> = sqlx::query_scalar(
        "SELECT active_uid FROM auth_write_admission
         WHERE singleton = 1 AND armed = 1 AND accepting = 1
           AND maintenance = 0 AND remote_writes = 0",
    )
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to read write admission: {error}"))?
    .flatten();
    if admitted.as_deref() != Some(principal) {
        return Ok(false);
    }
    let (foreign, unowned): (bool, bool) = sqlx::query_as(
        "SELECT
            EXISTS(SELECT 1 FROM venues
                   WHERE uid IS NOT NULL
                     AND id NOT IN (SELECT venue_id FROM auth_venue_access))
            OR EXISTS(SELECT 1 FROM tracks
                      WHERE uid IS NOT NULL
                        AND id NOT IN (SELECT track_id FROM auth_visible_tracks)),
            EXISTS(SELECT 1 FROM venues WHERE uid IS NULL)
            OR EXISTS(SELECT 1 FROM tracks WHERE uid IS NULL)",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to inspect the library's owner: {error}"))?;
    if !foreign {
        return Ok(false);
    }
    if unowned {
        log::warn!(
            "[sync] the library is another account's but also holds rows nobody owns; it is left as it is"
        );
        return Ok(false);
    }
    // Clearing drops the upload queue, and a local edit is not an eviction's
    // to discard.
    if !waiting_to_upload(&mut transaction).await?.is_empty() {
        log::warn!(
            "[sync] the library is another account's, but changes wait to upload; it is left as it is"
        );
        return Ok(false);
    }
    clear_synced_tables(&mut transaction).await?;
    crate::sync::triggers::uploading(&mut transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit the account clear: {error}"))?;
    log::info!("[sync] another account's library cleared for a fresh download");
    Ok(true)
}

/// Clear the synced tables if they hold an older projection, so PowerSync
/// downloads them again in the current format.
///
/// Runs before sync starts. The clear is marked local, so nothing it removes
/// reaches the server.
///
/// A change still waiting to upload was written in the old format, which the
/// server refuses and this build cannot translate, so it is never sent. It is
/// moved to the not-saved list (`sync_rejections`) in the same transaction as
/// the clear, so the copy is always cleared at a version change and the
/// person is told what did not make it.
///
/// # Errors
///
/// If the version or the queue cannot be read, or the clear fails.
pub(crate) async fn reset_stale_projection(pool: &SqlitePool) -> Result<(), String> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|error| format!("Failed to begin the projection check: {error}"))?;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to read the projection version: {error}"))?;
    if version == PROJECTION_VERSION {
        return Ok(());
    }
    let unsent = set_aside_waiting_uploads(&mut transaction).await?;
    if unsent > 0 {
        log::warn!(
            "[sync] {unsent} changes made at projection {version} were waiting to upload; they are listed as not saved"
        );
    }
    clear_synced_tables(&mut transaction).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "PRAGMA user_version = {PROJECTION_VERSION}"
    )))
    .execute(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to record the projection version: {error}"))?;
    crate::sync::triggers::uploading(&mut transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| format!("Failed to commit the projection reset: {error}"))?;
    log::info!("[sync] projection {version} cleared for a fresh download at {PROJECTION_VERSION}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build at a new projection cannot send what an older one queued, and
    /// must not keep the old copy waiting for it: at the version change every
    /// queued change is listed as not saved, the copy and the queue are
    /// cleared, and the library syncs again from nothing.
    #[tokio::test]
    async fn a_format_change_lists_queued_edits_as_not_saved_and_clears() {
        let directory = tempfile::tempdir().unwrap();
        let open = || async {
            let (_db, connections) = open_app_db_at(directory.path()).await.unwrap();
            crate::database::local::auth::arm_write_admission(connections.sql(), Some("user-1"))
                .await
                .unwrap();
            connections
                .start(
                    crate::sync::schema::schema(),
                    luma_sync::powersync::HttpClient::new(),
                )
                .await
                .unwrap()
        };
        let count = |pool: &SqlitePool, sql: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(sql)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };

        // A new library is itself behind the format until its first open,
        // before PowerSync has made its tables: that reset finds nothing.
        // Then an insert, an edit and a delete, queued and never uploaded.
        let library = open().await;
        assert_eq!(
            count(&library.sql, "SELECT COUNT(*) FROM sync_rejections").await,
            0
        );
        sqlx::raw_sql(
            "INSERT INTO venues (id, uid, name) VALUES ('venue-1', 'user-1', 'Room');
             INSERT INTO venues (id, uid, name) VALUES ('venue-2', 'user-1', 'Hall');
             UPDATE venues SET name = 'Club' WHERE id = 'venue-1';
             DELETE FROM venues WHERE id = 'venue-2';",
        )
        .execute(&library.sql)
        .await
        .unwrap();
        assert_eq!(count(&library.sql, "SELECT COUNT(*) FROM ps_crud").await, 4);
        // The library as an older build left it.
        sqlx::raw_sql("PRAGMA user_version = 0")
            .execute(&library.sql)
            .await
            .unwrap();
        library.close().await;

        let library = open().await;
        assert_eq!(count(&library.sql, "SELECT COUNT(*) FROM venues").await, 0);
        assert_eq!(count(&library.sql, "SELECT COUNT(*) FROM ps_crud").await, 0);
        assert_eq!(
            count(&library.sql, "PRAGMA user_version").await,
            PROJECTION_VERSION
        );
        let unsent: Vec<(String, String, String, Option<String>, String)> = sqlx::query_as(
            "SELECT table_name, row_id, op, data_json, body FROM sync_rejections ORDER BY rowid",
        )
        .fetch_all(&library.sql)
        .await
        .unwrap();
        assert_eq!(
            unsent
                .iter()
                .map(|(table, row, op, ..)| (table.as_str(), row.as_str(), op.as_str()))
                .collect::<Vec<_>>(),
            [
                ("venues", "venue-1", "PUT"),
                ("venues", "venue-2", "PUT"),
                ("venues", "venue-1", "PATCH"),
                ("venues", "venue-2", "DELETE"),
            ],
            "every queued change is listed, in the order it was made"
        );
        let edit: serde_json::Value =
            serde_json::from_str(unsent[2].3.as_deref().expect("the edit's columns")).unwrap();
        assert_eq!(edit["name"], "Club", "what the change was is kept");
        assert!(unsent[3].3.is_none(), "a delete carries no columns");
        assert!(unsent.iter().all(|(.., body)| body == PREDATES_UPDATE));

        // The cleared library is an ordinary one: a write queues, and opening
        // it again at the same version changes nothing.
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue-3', 'user-1', 'Loft')")
            .execute(&library.sql)
            .await
            .unwrap();
        library.close().await;
        let library = open().await;
        assert_eq!(count(&library.sql, "SELECT COUNT(*) FROM venues").await, 1);
        assert_eq!(count(&library.sql, "SELECT COUNT(*) FROM ps_crud").await, 1);
        assert_eq!(
            count(&library.sql, "SELECT COUNT(*) FROM sync_rejections").await,
            4
        );
        library.close().await;
    }

    /// A library as the squashed migrations left it: their schema, a row of
    /// data, and a history of the given versions.
    async fn library_at(directory: &Path, versions: &[i64]) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.join("luma.db"))
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/20261024000000_baseline.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE _sqlx_migrations (
                version BIGINT PRIMARY KEY,
                description TEXT NOT NULL,
                installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                success BOOLEAN NOT NULL,
                checksum BLOB NOT NULL,
                execution_time BIGINT NOT NULL
            );
            INSERT INTO venues (id, uid, name) VALUES ('hall', 'u', 'Hall');",
        )
        .execute(&pool)
        .await
        .unwrap();
        for version in versions {
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
                 VALUES (?1, 'squashed', TRUE, x'00', 1)",
            )
            .bind(version)
            .execute(&pool)
            .await
            .unwrap();
        }
        pool.close().await;
    }

    async fn history(db: &Db) -> Vec<(i64, Vec<u8>)> {
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&db.0)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_library_through_every_squashed_migration_adopts_the_baseline() {
        let directory = tempfile::tempdir().unwrap();
        library_at(directory.path(), &[20_251_220_125_200, LAST_SQUASHED]).await;

        let db = init_app_db_at(directory.path()).await.unwrap();
        let migrator = sqlx::migrate!("./migrations");
        let expected: Vec<(i64, Vec<u8>)> = migrator
            .iter()
            .map(|migration| (migration.version, migration.checksum.to_vec()))
            .collect();
        assert_eq!(history(&db).await, expected);
        let name: String = sqlx::query_scalar("SELECT name FROM venues WHERE id = 'hall'")
            .fetch_one(&db.0)
            .await
            .unwrap();
        assert_eq!(name, "Hall", "the library keeps its rows");
        db.0.close().await;

        let db = init_app_db_at(directory.path()).await.unwrap();
        assert_eq!(history(&db).await, expected, "and opens again unchanged");
    }

    #[tokio::test]
    async fn random_node_turns_every_saved_noise_node_into_a_gliding_random() {
        use luma_patterns::clip_graph::{check, ClipGraph, Input, Kind};
        use serde_json::{json, Value};

        const RANDOM_NODE: i64 = 20_261_025_120_000;
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePool::connect_with(
            SqliteConnectOptions::new()
                .filename(directory.path().join("luma.db"))
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .unwrap();
        sqlx::migrate!("./migrations")
            .run_to(RANDOM_NODE - 1, &pool)
            .await
            .unwrap();
        let noise = |nodes: Value| {
            let mut nodes = nodes;
            nodes["curve1"] = json!({"kind": "curve", "inputs": {"x": {"node": "noise1"}}});
            nodes["color1"] =
                json!({"kind": "color", "inputs": {"brightness": {"node": "curve1"}}});
            json!({"version": 3, "nodes": nodes})
        };
        let mut clouds = noise(json!({
            "noise1": {"kind": "noise", "inputs": {"period": 8.0, "scale": 0.4, "contrast": 0.6}},
            "noise2": {"kind": "noise", "inputs": {"period": {"node": "beats"}}},
            "beats": {"kind": "value", "inputs": {"value": 2.0}},
            "curve2": {"kind": "curve", "inputs": {"x": {"node": "noise2"}}}}));
        clouds["nodes"]["color1"]["inputs"]["alpha"] = json!({"node": "curve2"});
        let plain = noise(json!({"noise1": {"kind": "noise", "inputs": {}}}));
        let still =
            r#"{"version":3,"nodes":{"color1":{"kind":"color","inputs":{"brightness":0.5}}}}"#;
        // A draft's seed is past i64: it must come back digit for digit.
        let draft = json!({"clips": {"c-1": {"seed": 13369261119795889533_u64, "graph": clouds}}})
            .to_string();
        for (id, graph) in [("clouds", clouds.to_string()), ("plain", plain.to_string())]
            .into_iter()
            .chain([("still", still.to_owned())])
        {
            sqlx::query(
                "INSERT INTO clips (id, uid, score_id, graph, graph_json, start, duration, seed,
                                    selection_json, blend_mode)
                 VALUES (?, 'u', 's', '', ?, 0, 4, '7', '{}', 'replace')",
            )
            .bind(id)
            .bind(graph)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO drafts (id, uid, score_id, base_json, state_json) VALUES ('d', 'u', 's', ?, ?)",
        )
        .bind(&draft)
        .bind(&draft)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let db = init_app_db_at(directory.path()).await.unwrap();
        let graph = |id: &'static str| {
            let db = db.clone();
            async move {
                let text: String = sqlx::query_scalar("SELECT graph_json FROM clips WHERE id = ?")
                    .bind(id)
                    .fetch_one(&db.0)
                    .await
                    .unwrap();
                text
            }
        };
        let parsed = |text: &str| {
            let graph: ClipGraph = serde_json::from_str(text).unwrap();
            check(&graph).unwrap();
            graph
        };
        // The period became the random's every, which a later migration
        // moved into a clock of its own (random_time).
        let clouds = parsed(&graph("clouds").await);
        let every = |graph: &ClipGraph, random: &str| {
            let clock = graph.nodes[random].inputs["time"]
                .source()
                .unwrap()
                .to_owned();
            graph.nodes[&clock].inputs["every"].clone()
        };
        let first = &clouds.nodes["noise1"];
        assert_eq!(first.kind, Kind::Random);
        assert_eq!(every(&clouds, "noise1"), Input::Number(8.));
        assert_eq!(first.inputs["glide"], Input::Number(1.));
        assert_eq!(first.inputs["scale"], Input::Number(0.4));
        assert_eq!(first.inputs["contrast"], Input::Number(0.6));
        assert_eq!(every(&clouds, "noise2"), Input::wire("beats"));
        assert!(!clouds.nodes["noise2"].inputs.contains_key("period"));
        // No period is the same four beats as no every.
        let plain = parsed(&graph("plain").await);
        assert_eq!(plain.nodes["noise1"].kind, Kind::Random);
        assert_eq!(every(&plain, "noise1"), Input::Number(4.));
        assert_eq!(
            graph("still").await,
            still,
            "a graph with no noise is untouched"
        );

        let (base, state): (String, String) =
            sqlx::query_as("SELECT base_json, state_json FROM drafts WHERE id = 'd'")
                .fetch_one(&db.0)
                .await
                .unwrap();
        for text in [base, state] {
            assert!(text.contains("13369261119795889533"), "{text}");
            let clip: Value = serde_json::from_str::<Value>(&text).unwrap()["clips"]["c-1"].clone();
            assert_eq!(parsed(&clip["graph"].to_string()), clouds);
        }
    }

    #[tokio::test]
    async fn random_time_gives_each_saved_random_a_clock_of_its_every() {
        use luma_patterns::clip_graph::{check, ClipGraph, Input, Kind};
        use serde_json::{json, Value};

        const RANDOM_TIME: i64 = 20_261_107_130_000;
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePool::connect_with(
            SqliteConnectOptions::new()
                .filename(directory.path().join("luma.db"))
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .unwrap();
        sqlx::migrate!("./migrations")
            .run_to(RANDOM_TIME - 1, &pool)
            .await
            .unwrap();
        // Two randoms, one wired, beside a time node that holds time1, and
        // one with no every.
        let drift = json!({"version": 3, "nodes": {
            "time1": {"kind": "time", "inputs": {"every": 1}},
            "pulse": {"kind": "curve", "inputs": {"x": {"node": "time1"}}},
            "beats": {"kind": "value", "inputs": {"value": 2.0}},
            "a": {"kind": "random", "inputs": {"every": 8.0, "glide": 1.0, "scale": 0.3}},
            "b": {"kind": "random", "inputs": {"every": {"node": "beats"}}},
            "c": {"kind": "random"},
            "ca": {"kind": "curve", "inputs": {"x": {"node": "a"}}},
            "cb": {"kind": "curve", "inputs": {"x": {"node": "b"}}},
            "cc": {"kind": "curve", "inputs": {"x": {"node": "c"}}},
            "product": {"kind": "math", "inputs": {"values": [
                {"node": "pulse"}, {"node": "ca"}, {"node": "cb"}, {"node": "cc"}]}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "product"}}}}});
        let still =
            r#"{"version":3,"nodes":{"color1":{"kind":"color","inputs":{"brightness":0.5}}}}"#;
        for (id, graph) in [("drift", drift.to_string()), ("still", still.to_owned())] {
            sqlx::query(
                "INSERT INTO clips (id, uid, score_id, graph, graph_json, start, duration, seed,
                                    selection_json, blend_mode)
                 VALUES (?, 'u', 's', '', ?, 0, 4, '7', '{}', 'replace')",
            )
            .bind(id)
            .bind(graph)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO perform_patterns (id, uid, venue_id, name, selection_json, z_index,
                                           blend_mode, seed, graph_json)
             VALUES ('p', 'u', 'hall', 'Drift', '{}', 0, 'replace', '7', ?)",
        )
        .bind(drift.to_string())
        .execute(&pool)
        .await
        .unwrap();
        // A draft's seed is past i64: it must come back digit for digit.
        let draft = json!({"clips": {"c-1": {"seed": 13369261119795889533_u64, "graph": drift}}})
            .to_string();
        sqlx::query(
            "INSERT INTO drafts (id, uid, score_id, base_json, state_json) VALUES ('d', 'u', 's', ?, ?)",
        )
        .bind(&draft)
        .bind(&draft)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let db = init_app_db_at(directory.path()).await.unwrap();
        let read = |query: &'static str| {
            let db = db.clone();
            async move {
                let text: String = sqlx::query_scalar(query).fetch_one(&db.0).await.unwrap();
                text
            }
        };
        let parsed = |text: &str| {
            let graph: ClipGraph = serde_json::from_str(text).unwrap();
            check(&graph).unwrap();
            graph
        };
        let drift = parsed(&read("SELECT graph_json FROM clips WHERE id = 'drift'").await);
        let clock = |random: &str| {
            let node = &drift.nodes[random];
            assert_eq!(node.kind, Kind::Random);
            assert!(
                !node.inputs.contains_key("every"),
                "{random} keeps no every"
            );
            let clock = node.inputs["time"].source().unwrap().to_owned();
            (clock.clone(), drift.nodes[&clock].inputs["every"].clone())
        };
        assert_eq!(
            clock("a"),
            ("time2".into(), Input::Number(8.)),
            "time1 is taken"
        );
        assert_eq!(clock("b"), ("time3".into(), Input::wire("beats")));
        assert_eq!(
            clock("c"),
            ("time4".into(), Input::Number(4.)),
            "no every was 4 beats"
        );
        assert_eq!(drift.nodes["a"].inputs["glide"], Input::Number(1.));
        assert_eq!(
            read("SELECT graph_json FROM clips WHERE id = 'still'").await,
            still,
            "a graph with no random is untouched"
        );
        assert_eq!(
            parsed(&read("SELECT graph_json FROM perform_patterns WHERE id = 'p'").await),
            drift
        );
        for column in [
            "SELECT base_json FROM drafts WHERE id = 'd'",
            "SELECT state_json FROM drafts WHERE id = 'd'",
        ] {
            let text = read(column).await;
            assert!(text.contains("13369261119795889533"), "{text}");
            let clip: Value = serde_json::from_str::<Value>(&text).unwrap()["clips"]["c-1"].clone();
            assert_eq!(parsed(&clip["graph"].to_string()), drift);
        }
    }

    #[tokio::test]
    async fn an_older_library_is_refused_and_left_alone() {
        let directory = tempfile::tempdir().unwrap();
        // The last migration Luma 0.5.19 shipped.
        library_at(directory.path(), &[20_260_403_200_000]).await;

        let error = init_app_db_at(directory.path()).await.err().unwrap();
        assert!(error.contains("cannot upgrade"), "{error}");

        let pool = SqlitePool::connect_with(
            SqliteConnectOptions::new().filename(directory.path().join("luma.db")),
        )
        .await
        .unwrap();
        let versions: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(versions, vec![20_260_403_200_000]);
    }
}
