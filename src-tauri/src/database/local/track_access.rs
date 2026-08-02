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
