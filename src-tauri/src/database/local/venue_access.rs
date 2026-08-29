use std::marker::PhantomData;

use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

pub struct Read;
pub struct Write;
pub struct Operate;

/// A fixed route from a known descendant ID to its venue root. Callers cannot
/// authorize one venue and then operate on a resource belonging to another.
#[derive(Clone, Copy, Debug)]
pub enum VenueResource<'a> {
    Venue(&'a str),
    Fixture(&'a str),
    Group(&'a str),
    GroupMember(&'a str),
    StagePiece(&'a str),
    Cue(&'a str),
    MidiModifier(&'a str),
    MidiBinding(&'a str),
    Score(&'a str),
    TrackScore(&'a str),
    AgentThread(&'a str),
}

/// Opaque, transaction-bound proof that the app database admitted the current
/// host principal to one venue. Mutations use an IMMEDIATE transaction so the
/// proof and write serialize with identity switches; reads keep one deferred
/// snapshot and may finish if they were admitted before a switch began.
pub struct VenueAccess<'a, Mode> {
    transaction: Transaction<'a, Sqlite>,
    venue_id: String,
    principal: Option<String>,
    _mode: PhantomData<Mode>,
}

mod sealed {
    pub trait Sealed {}
}

/// Implemented only by guards created in this module. Read helpers may accept
/// either a read snapshot or the write transaction already held by a mutation.
pub trait AuthorizedVenue: sealed::Sealed {
    fn venue_id(&self) -> &str;
    fn principal(&self) -> Option<&str>;
    fn connection(&mut self) -> &mut SqliteConnection;
}

impl<Mode> sealed::Sealed for VenueAccess<'_, Mode> {}

impl<Mode> AuthorizedVenue for VenueAccess<'_, Mode> {
    fn venue_id(&self) -> &str {
        &self.venue_id
    }

    fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    fn connection(&mut self) -> &mut SqliteConnection {
        &mut self.transaction
    }
}

impl<'a> VenueAccess<'a, Read> {
    pub async fn read(pool: &'a SqlitePool, resource: VenueResource<'_>) -> Result<Self, String> {
        let transaction = pool
            .begin()
            .await
            .map_err(|error| format!("Failed to begin venue access: {error}"))?;
        authorize(transaction, resource, false).await
    }
}

impl<'a> VenueAccess<'a, Write> {
    pub async fn write(pool: &'a SqlitePool, resource: VenueResource<'_>) -> Result<Self, String> {
        let transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| format!("Failed to begin venue mutation: {error}"))?;
        authorize(transaction, resource, true).await
    }

    pub async fn commit(self) -> Result<(), String> {
        self.transaction
            .commit()
            .await
            .map_err(|error| format!("Failed to commit venue mutation: {error}"))
    }

    /// Enter transaction-local maintenance for a cascade that has already
    /// been authorized through this guard. The caller must leave maintenance
    /// before commit; otherwise the transaction rolls back when dropped.
    pub(crate) async fn enter_maintenance(&mut self) -> Result<(), String> {
        crate::database::local::write_admission::enter_maintenance_writes(
            &mut self.transaction,
            self.principal.as_deref(),
        )
        .await
    }

    pub(crate) async fn leave_maintenance(&mut self) -> Result<(), String> {
        crate::database::local::write_admission::leave_maintenance_writes(
            &mut self.transaction,
            self.principal.as_deref(),
        )
        .await
    }
}

impl<'a> VenueAccess<'a, Operate> {
    /// Serialize an ephemeral live/device effect with identity transitions
    /// without granting persistent owner mutation. Members are operators; the
    /// IMMEDIATE transaction keeps their exact admission stable until commit.
    pub async fn operate(
        pool: &'a SqlitePool,
        resource: VenueResource<'_>,
    ) -> Result<Self, String> {
        let transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| format!("Failed to begin venue operation: {error}"))?;
        authorize(transaction, resource, false).await
    }

    pub async fn commit(self) -> Result<(), String> {
        self.transaction
            .commit()
            .await
            .map_err(|error| format!("Failed to commit venue operation: {error}"))
    }
}

impl<Mode> VenueAccess<'_, Mode> {
    pub fn venue_id(&self) -> &str {
        &self.venue_id
    }

    pub fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    pub fn require_venue(&self, venue_id: &str) -> Result<(), String> {
        if self.venue_id == venue_id {
            Ok(())
        } else {
            Err(not_found())
        }
    }
}

async fn authorize<'a, Mode>(
    mut transaction: Transaction<'a, Sqlite>,
    resource: VenueResource<'_>,
    owner_only: bool,
) -> Result<VenueAccess<'a, Mode>, String> {
    let venue_id = resolve_venue_id(&mut transaction, resource)
        .await?
        .ok_or_else(not_found)?;

    let row: Option<(Option<String>, String, i64, i64, i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT venue.uid, venue.role,
                    admission.armed, admission.accepting, admission.maintenance,
                    admission.active_uid,
                    EXISTS(
                        SELECT 1 FROM venue_memberships membership
                        WHERE membership.venue_id = venue.id
                          AND membership.user_id = admission.active_uid
                          AND membership.role = 'member'
                    )
             FROM venues venue
             CROSS JOIN auth_write_admission admission
             WHERE venue.id = ? AND admission.singleton = 1",
    )
    .bind(&venue_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to authorize venue access: {error}"))?;
    let Some((owner_uid, role, armed, accepting, maintenance, active_uid, is_member)) = row else {
        return Err(not_found());
    };
    if armed != 1 || accepting != 1 || maintenance != 0 {
        return Err(not_found());
    }

    let principal = active_uid;
    let guest_venue = owner_uid.is_none() && role != "member";
    let owner = match principal.as_deref() {
        Some(principal) => owner_uid.as_deref() == Some(principal),
        None => guest_venue,
    };
    let member = principal.is_some() && is_member == 1;
    let allowed = if owner_only { owner } else { owner || member };
    if !allowed {
        return Err(not_found());
    }

    Ok(VenueAccess {
        transaction,
        venue_id,
        principal,
        _mode: PhantomData,
    })
}

async fn resolve_venue_id(
    transaction: &mut Transaction<'_, Sqlite>,
    resource: VenueResource<'_>,
) -> Result<Option<String>, String> {
    let (sql, id) = match resource {
        VenueResource::Venue(id) => ("SELECT id FROM venues WHERE id = ?", id),
        VenueResource::Fixture(id) => ("SELECT venue_id FROM fixtures WHERE id = ?", id),
        VenueResource::Group(id) => ("SELECT venue_id FROM fixture_groups WHERE id = ?", id),
        VenueResource::GroupMember(id) => (
            "SELECT groups.venue_id
             FROM fixture_group_members member
             JOIN fixture_groups groups ON groups.id = member.group_id
             WHERE member.id = ?",
            id,
        ),
        VenueResource::StagePiece(id) => ("SELECT venue_id FROM stage_pieces WHERE id = ?", id),
        VenueResource::Cue(id) => ("SELECT venue_id FROM cues WHERE id = ?", id),
        VenueResource::MidiModifier(id) => ("SELECT venue_id FROM midi_modifiers WHERE id = ?", id),
        VenueResource::MidiBinding(id) => ("SELECT venue_id FROM midi_bindings WHERE id = ?", id),
        VenueResource::Score(id) => ("SELECT venue_id FROM scores WHERE id = ?", id),
        VenueResource::TrackScore(id) => (
            "SELECT score.venue_id
             FROM track_scores clip
             JOIN scores score ON score.id = clip.score_id
             WHERE clip.id = ?",
            id,
        ),
        VenueResource::AgentThread(id) => ("SELECT venue_id FROM agent_threads WHERE id = ?", id),
    };
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|error| format!("Failed to resolve venue resource: {error}"))
}

fn not_found() -> String {
    "Venue resource not found".into()
}

// -----------------------------------------------------------------------------
// What a venue owns
// -----------------------------------------------------------------------------

/// Tables that carry a `venue_id` and are still not a venue's *content*.
///
/// The only thing about venue ownership that has to be written down. Everything
/// else is derived — see [`venue_owned_tables`].
const NOT_VENUE_CONTENT: &[&str] = &[
    // Who may open the venue, not what is in it. A membership is the venue's
    // access-control row, and it goes when the venue goes.
    "venue_memberships",
];

/// Every table whose rows are one venue's content, read off the live schema.
///
/// A `venue_id` column is what makes a row belong to a venue, so the schema
/// already holds this list; a second hand-maintained copy is a list that
/// drifts, and it did — `sync::pull` guarded a remote venue delete on
/// `stage_pieces` and never learned about `venue_nodes`, so a venue built
/// entirely as a graph was cascade-deleted. Adding a venue-scoped table now
/// costs nothing here, and forgetting one is no longer possible.
///
/// In table-name order, so callers that build SQL from it build the same SQL
/// every time.
///
/// # Errors
/// Fails if the schema cannot be read.
pub async fn venue_owned_tables(connection: &mut SqliteConnection) -> Result<Vec<String>, String> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT schema.name
         FROM sqlite_schema schema
         JOIN pragma_table_info(schema.name) column ON column.name = 'venue_id'
         WHERE schema.type = 'table'
         ORDER BY schema.name",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| format!("Failed to read venue-owned tables: {error}"))?;
    Ok(tables
        .into_iter()
        .filter(|table| !NOT_VENUE_CONTENT.contains(&table.as_str()))
        .collect())
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    use super::*;

    /// The registry is the schema's answer, so a new venue-scoped table is in
    /// it the day it is created and nobody has to remember a second list.
    #[tokio::test]
    async fn venue_owned_tables_are_read_off_the_schema() {
        let (_directory, pool) = test_pool().await;
        let mut connection = pool.acquire().await.unwrap();
        let tables = venue_owned_tables(&mut connection).await.unwrap();

        for owned in ["fixtures", "stage_pieces", "venue_nodes", "scores", "cues"] {
            assert!(
                tables.iter().any(|t| t == owned),
                "{owned} carries a venue_id and is venue content: {tables:?}"
            );
        }
        // The venue row itself is not its own content, and a membership is who
        // may open the venue rather than what is in it.
        for excluded in ["venues", "venue_memberships"] {
            assert!(
                !tables.iter().any(|t| t == excluded),
                "{excluded} is not venue content: {tables:?}"
            );
        }
        let mut sorted = tables.clone();
        sorted.sort();
        assert_eq!(tables, sorted, "callers build SQL from this order");
    }

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.path().join("venue-access.db"))
                    .journal_mode(SqliteJournalMode::Wal)
                    .create_if_missing(true)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO venues (id, uid, name) VALUES
                ('alice', 'alice', 'Alice'),
                ('shared', 'alice', 'Shared');
             INSERT INTO fixtures
                (id, uid, venue_id, address, num_channels, manufacturer, model, mode_name, fixture_path)
                VALUES
                ('alice-fixture', 'alice', 'alice', 1, 1, 'Test', 'Alice', 'Default', 'alice.json'),
                ('shared-fixture', 'alice', 'shared', 2, 1, 'Test', 'Shared', 'Default', 'shared.json');
             INSERT INTO fixture_groups (id, uid, venue_id, name)
                VALUES ('shared-group', 'alice', 'shared', 'shared_group');
             INSERT INTO fixture_group_members (id, uid, fixture_id, group_id)
                VALUES ('shared-member', 'alice', 'shared-fixture', 'shared-group');
             INSERT INTO cues (id, uid, venue_id, name, pattern_id)
                VALUES ('shared-cue', 'alice', 'shared', 'Shared cue', 'pattern');
             INSERT INTO midi_modifiers (id, uid, venue_id, name, input_json)
                VALUES ('shared-modifier', 'alice', 'shared', 'Shift', '{}');
             INSERT INTO stage_pieces (id, uid, venue_id, mesh_path, kind, label)
                VALUES ('alice-stage', 'alice', 'alice', 'booth.glb', 'stand', 'Booth')",
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();
        let mut membership = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        crate::database::local::write_admission::enter_remote_writes(&mut membership)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO venue_memberships (venue_id, user_id, role)
             VALUES ('shared', 'bob', 'member')",
        )
        .execute(&mut *membership)
        .await
        .unwrap();
        crate::database::local::write_admission::leave_remote_writes(&mut membership)
            .await
            .unwrap();
        membership.commit().await.unwrap();
        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('guest', NULL, 'Guest')")
            .execute(&pool)
            .await
            .unwrap();
        (directory, pool)
    }

    #[tokio::test]
    async fn members_are_read_only_while_owners_and_guests_can_write_their_aggregate() {
        let (_directory, pool) = test_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        VenueAccess::<Write>::write(&pool, VenueResource::StagePiece("alice-stage"))
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();

        crate::database::local::auth::arm_write_admission(&pool, Some("bob"))
            .await
            .unwrap();
        assert!(
            VenueAccess::<Read>::read(&pool, VenueResource::Venue("shared"))
                .await
                .is_ok()
        );
        assert!(
            VenueAccess::<Read>::read(&pool, VenueResource::Fixture("shared-fixture"))
                .await
                .is_ok()
        );
        assert!(
            VenueAccess::<Read>::read(&pool, VenueResource::GroupMember("shared-member"))
                .await
                .is_ok()
        );
        assert!(
            VenueAccess::<Write>::write(&pool, VenueResource::Venue("shared"))
                .await
                .is_err()
        );
        assert!(
            VenueAccess::<Write>::write(&pool, VenueResource::Fixture("shared-fixture"))
                .await
                .is_err()
        );
        assert!(
            VenueAccess::<Write>::write(&pool, VenueResource::Group("shared-group"))
                .await
                .is_err()
        );
        assert!(
            VenueAccess::<Write>::write(&pool, VenueResource::Cue("shared-cue"))
                .await
                .is_err()
        );
        assert!(
            VenueAccess::<Write>::write(&pool, VenueResource::MidiModifier("shared-modifier"),)
                .await
                .is_err()
        );
        let trigger_error =
            sqlx::query("UPDATE fixtures SET label = 'member bypass' WHERE id = 'shared-fixture'")
                .execute(&pool)
                .await
                .unwrap_err();
        assert!(trigger_error
            .to_string()
            .contains("fixture write is not authorized"));
        let grant_error = sqlx::query(
            "INSERT INTO venue_memberships (venue_id, user_id, role)
             VALUES ('alice', 'bob', 'member')",
        )
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(grant_error
            .to_string()
            .contains("venue membership grant is not authorized"));
        assert!(
            VenueAccess::<Read>::read(&pool, VenueResource::Fixture("alice-fixture"),)
                .await
                .is_err()
        );

        sqlx::query(
            "UPDATE auth_write_admission
             SET accepting = 0, active_uid = NULL, maintenance = 0, remote_writes = 0
             WHERE singleton = 1",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            VenueAccess::<Read>::read(&pool, VenueResource::Venue("guest"))
                .await
                .is_err()
        );

        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .unwrap();
        assert!(
            VenueAccess::<Write>::write(&pool, VenueResource::Venue("guest"),)
                .await
                .is_ok()
        );
        assert!(
            VenueAccess::<Read>::read(&pool, VenueResource::Fixture("alice-fixture"),)
                .await
                .is_err()
        );

        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        assert!(
            VenueAccess::<Write>::write(&pool, VenueResource::Cue("shared-cue"))
                .await
                .is_ok()
        );
        sqlx::query("UPDATE fixtures SET label = 'owner edit' WHERE id = 'shared-fixture'")
            .execute(&pool)
            .await
            .unwrap();
    }
}
