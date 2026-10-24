use luma_sync::powersync::Connections;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use std::path::Path;

/// `SqlitePool` is itself a shared handle, so cloning `Db` hands out another
/// reference to the same pool, never a second pool.
#[derive(Clone)]
pub struct Db(pub SqlitePool);

/// Create the directory, upgrade legacy payloads and run every migration.
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

    Ok((Db(connections.sql().clone()), connections))
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
