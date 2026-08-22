use std::marker::PhantomData;

use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

pub struct Read;
pub struct Operate;

/// Transaction-bound proof that one track is visible through the canonical
/// `auth_visible_tracks` capability. File/audio readers snapshot every path
/// through this connection; live operations use an IMMEDIATE transaction so
/// an account switch cannot cross the final effect boundary.
pub struct VisibleTrackAccess<'a, Mode> {
    transaction: Transaction<'a, Sqlite>,
    track_id: String,
    principal: Option<String>,
    _mode: PhantomData<Mode>,
}

impl<'a> VisibleTrackAccess<'a, Read> {
    pub async fn read(pool: &'a SqlitePool, track_id: &str) -> Result<Self, String> {
        let transaction = pool
            .begin()
            .await
            .map_err(|error| format!("Failed to begin track read: {error}"))?;
        authorize(transaction, track_id).await
    }

    /// Finish the visibility snapshot and wait until SQLx has returned its
    /// connection to the pool. Dropping a transaction starts rollback in the
    /// background; callers that immediately open another capability need this
    /// explicit boundary so a one-connection pool cannot wait on itself.
    pub async fn finish(self) -> Result<(), String> {
        self.transaction
            .rollback()
            .await
            .map_err(|error| format!("Failed to finish track read: {error}"))
    }
}

impl<'a> VisibleTrackAccess<'a, Operate> {
    pub async fn operate(pool: &'a SqlitePool, track_id: &str) -> Result<Self, String> {
        let transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| format!("Failed to begin track operation: {error}"))?;
        authorize(transaction, track_id).await
    }

    pub async fn commit(self) -> Result<(), String> {
        self.transaction
            .commit()
            .await
            .map_err(|error| format!("Failed to commit track operation: {error}"))
    }
}

impl<Mode> VisibleTrackAccess<'_, Mode> {
    pub fn track_id(&self) -> &str {
        &self.track_id
    }

    pub fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    pub fn connection(&mut self) -> &mut SqliteConnection {
        &mut self.transaction
    }
}

async fn authorize<'a, Mode>(
    mut transaction: Transaction<'a, Sqlite>,
    track_id: &str,
) -> Result<VisibleTrackAccess<'a, Mode>, String> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT visible.track_id, admission.active_uid
         FROM auth_visible_tracks visible
         CROSS JOIN auth_write_admission admission
         WHERE visible.track_id = ? AND admission.singleton = 1",
    )
    .bind(track_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to authorize track access: {error}"))?;
    let Some((track_id, principal)) = row else {
        return Err("Track not found".into());
    };
    Ok(VisibleTrackAccess {
        transaction,
        track_id,
        principal,
        _mode: PhantomData,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::time::Duration;

    #[tokio::test]
    async fn a_finished_read_can_be_immediately_reacquired_from_a_single_connection_pool() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE auth_visible_tracks (track_id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE auth_write_admission (singleton INTEGER PRIMARY KEY, active_uid TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO auth_visible_tracks (track_id) VALUES ('track-a')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO auth_write_admission (singleton, active_uid) VALUES (1, NULL)")
            .execute(&pool)
            .await
            .unwrap();

        let first = VisibleTrackAccess::<Read>::read(&pool, "track-a")
            .await
            .unwrap();
        first.finish().await.unwrap();

        let second = tokio::time::timeout(
            Duration::from_secs(1),
            VisibleTrackAccess::<Read>::read(&pool, "track-a"),
        )
        .await
        .expect("finished read did not return its only connection")
        .unwrap();
        second.finish().await.unwrap();
    }
}
