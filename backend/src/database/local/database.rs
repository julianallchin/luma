use luma_sync::powersync::Connections;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use std::path::Path;

/// The format of the synced tables' local copy.
///
/// Bump it when a client can no longer read rows an older one downloaded. A
/// library whose `user_version` differs clears its synced tables at startup,
/// keeping its upload queue, and PowerSync downloads them again — so no code
/// ever converts a local row. Migrations set the version their schema reads.
pub const PROJECTION_VERSION: i64 = 1;

/// `SqlitePool` is itself a shared handle, so cloning `Db` hands out another
/// reference to the same pool, never a second pool.
#[derive(Clone)]
pub struct Db(pub SqlitePool);

/// Create the directory and run every migration.
///
/// Migrations run on their own connection with foreign keys off — several of
/// them rebuild a table and would trip over their own child rows otherwise —
/// and that connection is closed before anything else opens the file. Returns
/// where the database is.
async fn migrate_app_db_at(app_dir: &Path) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(app_dir).map_err(|error| {
        format!(
            "Failed to create app config dir {}: {error}",
            app_dir.display()
        )
    })?;
    let db_path = app_dir.join("luma.db");
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
    migrate_pool.close().await;
    Ok(db_path)
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
    let db_path = migrate_app_db_at(app_dir).await?;
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
    let db_path = migrate_app_db_at(app_dir).await?;

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
    reset_stale_projection(connections.sql()).await?;

    Ok((Db(connections.sql().clone()), connections))
}

/// Clear the synced tables if they hold an older projection, so PowerSync
/// downloads them again in the current format.
///
/// Runs before sync starts. The clear is marked local, so nothing it removes
/// reaches the server. A change still waiting to upload postpones the reset to
/// a later launch: clearing PowerSync's download state drops its queue, and
/// a local edit is not this function's to discard.
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
    // PowerSync creates its tables the first time it opens a library.
    let synced_before: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ps_buckets')",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to inspect the sync state: {error}"))?;
    if synced_before {
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ps_crud")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| format!("Failed to count changes waiting to sync: {error}"))?;
        if pending > 0 {
            log::warn!(
                "[sync] projection {version} is not {PROJECTION_VERSION}; {pending} change(s) wait to upload, so the re-download waits for a later launch"
            );
            return Ok(());
        }
    }
    crate::sync::triggers::local_only(&mut transaction).await?;
    // Every synced table goes, so a reference between two of them is gone by
    // the commit whichever order they are cleared in. Admission guards a write
    // made *as* someone; an eviction is nobody's, so it is disarmed for the
    // clear and put back as it was.
    let armed: Option<i64> =
        sqlx::query_scalar("SELECT armed FROM auth_write_admission WHERE singleton = 1")
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| format!("Failed to read write admission: {error}"))?;
    sqlx::raw_sql(
        "PRAGMA defer_foreign_keys = ON;
         UPDATE auth_write_admission SET armed = 0 WHERE singleton = 1;",
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to prepare the projection reset: {error}"))?;
    for table in crate::sync::schema::SYNCED_TABLES.iter().rev() {
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {}", table.name)))
            .execute(&mut *transaction)
            .await
            .map_err(|error| format!("Failed to clear {}: {error}", table.name))?;
    }
    sqlx::query("UPDATE auth_write_admission SET armed = ? WHERE singleton = 1")
        .bind(armed.unwrap_or(0))
        .execute(&mut *transaction)
        .await
        .map_err(|error| format!("Failed to restore write admission: {error}"))?;
    if synced_before {
        sqlx::query("SELECT powersync_clear(0)")
            .execute(&mut *transaction)
            .await
            .map_err(|error| format!("Failed to reset the sync state: {error}"))?;
    }
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
        let clouds = parsed(&graph("clouds").await);
        let first = &clouds.nodes["noise1"];
        assert_eq!(first.kind, Kind::Random);
        assert_eq!(first.inputs["every"], Input::Number(8.));
        assert_eq!(first.inputs["glide"], Input::Number(1.));
        assert_eq!(first.inputs["scale"], Input::Number(0.4));
        assert_eq!(first.inputs["contrast"], Input::Number(0.6));
        let wired = &clouds.nodes["noise2"];
        assert_eq!(wired.inputs["every"], Input::wire("beats"));
        assert!(!wired.inputs.contains_key("period"));
        // No period is the same four beats as no every.
        let plain = parsed(&graph("plain").await);
        assert_eq!(plain.nodes["noise1"].kind, Kind::Random);
        assert!(!plain.nodes["noise1"].inputs.contains_key("every"));
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
