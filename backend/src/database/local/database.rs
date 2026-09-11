use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use std::path::Path;

/// `SqlitePool` is itself a shared handle, so cloning `Db` hands out another
/// reference to the same pool, never a second pool.
#[derive(Clone)]
pub struct Db(pub SqlitePool);

pub async fn init_app_db_at(app_dir: &Path) -> Result<Db, String> {
    std::fs::create_dir_all(app_dir).map_err(|e| {
        format!(
            "Failed to create app config dir {}: {}",
            app_dir.display(),
            e
        )
    })?;

    let db_path = app_dir.join("luma.db");
    // Connect WITHOUT foreign_keys for migrations (some migrations need FK checks off)
    let migrate_options = SqliteConnectOptions::new()
        .filename(&db_path)
        .journal_mode(SqliteJournalMode::Wal)
        .create_if_missing(true)
        .foreign_keys(false);

    let migrate_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(migrate_options)
        .await
        .map_err(|e| {
            format!(
                "Failed to connect to database at {}: {}",
                db_path.display(),
                e
            )
        })?;

    // Some relational graph payloads predate the current canonical document
    // format. Upgrade those explicit legacy shapes before migrations install
    // admission guards and before authored-state root import.
    super::legacy_graph_upgrade::upgrade_legacy_graph_json(&migrate_pool).await?;

    sqlx::migrate!("./migrations")
        .run(&migrate_pool)
        .await
        .map_err(|e| format!("Failed to run app migrations: {}", e))?;

    // The row-model migration parks every stored score document rather than
    // splitting it in SQL — see `scores::rows::cutover`.
    super::scores::rows::cutover(&migrate_pool).await?;

    migrate_pool.close().await;

    // Now open the real pool WITH foreign_keys enabled
    let connect_options = SqliteConnectOptions::new()
        .filename(&db_path)
        .journal_mode(SqliteJournalMode::Wal)
        .create_if_missing(true)
        .foreign_keys(true);

    // Every pooled connection is a writer, so every one carries the change log
    // and the PowerSync upload queue. Both trigger sets are TEMP, so they
    // belong to the connection and never to the file: the sync SDK holds its
    // own pool and deliberately has neither, which is what keeps a downloaded
    // row out of this database's history and out of the upload queue.
    let pool = SqlitePoolOptions::new()
        .max_connections(16)
        .after_connect(|connection, _| {
            Box::pin(async move {
                crate::sync::triggers::install(connection)
                    .await
                    .map_err(|error| sqlx::Error::Configuration(error.into()))
            })
        })
        .connect_with(connect_options)
        .await
        .map_err(|e| {
            format!(
                "Failed to connect to database at {}: {}",
                db_path.display(),
                e
            )
        })?;

    Ok(Db(pool))
}

#[cfg(test)]
mod tests {
    /// The row-model cutover against a real library, run only when one is
    /// pointed at: `LUMA_MIGRATION_DB=/path/to/a/copy/of/luma.db`.
    ///
    /// It is a copy on purpose. The test migrates the file it is given, and a
    /// migration is not a thing to try on the only copy.
    #[tokio::test]
    #[ignore = "needs a copy of a real library via LUMA_MIGRATION_DB"]
    async fn a_real_library_migrates_and_its_scores_become_rows() {
        let Ok(source) = std::env::var("LUMA_MIGRATION_DB") else {
            return;
        };
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::copy(&source, directory.path().join("luma.db")).expect("copy the library");

        let legacy_before = count(&source, "SELECT COUNT(*) FROM scores WHERE graph_document_json IS NULL AND EXISTS(SELECT 1 FROM track_scores WHERE score_id = scores.id)").await;
        let graph_before = count(
            &source,
            "SELECT COUNT(*) FROM scores WHERE graph_document_json IS NOT NULL",
        )
        .await;

        let db = super::init_app_db_at(directory.path())
            .await
            .expect("migrate the library");

        let one = |sql: &'static str| {
            let pool = db.0.clone();
            async move {
                sqlx::query_scalar::<_, i64>(sql)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(
            one("SELECT COUNT(*) FROM legacy_scores_backup").await,
            legacy_before,
            "every legacy score is kept in the backup table"
        );
        assert_eq!(
            one("SELECT COUNT(*) FROM scores").await,
            graph_before,
            "a score with no document and no clips is gone"
        );
        assert_eq!(
            one("SELECT COUNT(*) FROM scores_pending_cutover").await,
            0,
            "startup finishes the conversion the migration parked"
        );
        assert!(
            one("SELECT COUNT(*) FROM clips").await > 0,
            "the converted documents produced clips"
        );
        // Printed, not asserted: the shape of one real library is a fact about
        // that library, and a number here would make this a test of the file.
        eprintln!(
            "[migration] scores={} clips={} definitions={} legacy_backup={}",
            one("SELECT COUNT(*) FROM scores").await,
            one("SELECT COUNT(*) FROM clips").await,
            one("SELECT COUNT(*) FROM score_definitions").await,
            one("SELECT COUNT(*) FROM legacy_scores_backup").await,
        );
        assert!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(&db.0)
                .await
                .unwrap()
                .is_empty(),
            "the migrated database has no dangling references"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
                .fetch_one(&db.0)
                .await
                .unwrap(),
            "ok"
        );
    }

    async fn count(path: &str, sql: &'static str) -> i64 {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{path}?mode=ro"))
            .await
            .expect("open the source library read-only");
        let value = sqlx::query_scalar::<_, i64>(sql)
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        value
    }
}
