use std::marker::PhantomData;

use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

pub struct Read;
/// A persistent edit of the venue's content: its fixtures, groups, stage,
/// scores, environment and the rest. The owner and every member may make one;
/// the server's `public.history` records who did.
pub struct Write;
/// A change to who has the venue — its share code and its memberships. The
/// owner only.
pub struct Owner;

/// A fixed route from a known descendant ID to its venue root. Callers cannot
/// authorize one venue and then operate on a resource belonging to another.
#[derive(Clone, Copy, Debug)]
pub enum VenueResource<'a> {
    Venue(&'a str),
    Fixture(&'a str),
    Group(&'a str),
    Folder(&'a str),
    GroupMember(&'a str),
    Score(&'a str),
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
    /// Whether the writes are an agent's, and so must be given back to the
    /// person before commit. See [`crate::sync::triggers::attribute`].
    attributed: bool,
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
        Self::write_as(pool, resource, None).await
    }

    /// A mutation made on `actor`'s behalf — a model id or an MCP client's
    /// label — so the server's history names it instead of the person. `None`
    /// is the person, as [`Self::write`].
    pub async fn write_as(
        pool: &'a SqlitePool,
        resource: VenueResource<'_>,
        actor: Option<&str>,
    ) -> Result<Self, String> {
        let transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| format!("Failed to begin venue mutation: {error}"))?;
        let mut access: Self = authorize(transaction, resource, false).await?;
        if let Some(actor) = actor {
            crate::sync::triggers::attribute(&mut access.transaction, actor).await?;
            access.attributed = true;
        }
        Ok(access)
    }

    pub async fn commit(mut self) -> Result<(), String> {
        if self.attributed {
            crate::sync::triggers::unattribute(&mut self.transaction).await?;
        }
        self.transaction
            .commit()
            .await
            .map_err(|error| format!("Failed to commit venue mutation: {error}"))
    }
}

impl<'a> VenueAccess<'a, Owner> {
    /// Admit the venue's owner, and nobody else, to change who has the venue.
    pub async fn owner(pool: &'a SqlitePool, resource: VenueResource<'_>) -> Result<Self, String> {
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

    let row: Option<(Option<String>, i64, i64, i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT venue.uid,
                    admission.armed, admission.accepting, admission.maintenance,
                    admission.active_uid,
                    EXISTS(
                        SELECT 1 FROM venue_members membership
                        WHERE membership.venue_id = venue.id
                          AND membership.uid = admission.active_uid
                    )
             FROM venues venue
             CROSS JOIN auth_write_admission admission
             WHERE venue.id = ? AND admission.singleton = 1",
    )
    .bind(&venue_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to authorize venue access: {error}"))?;
    let Some((owner_uid, armed, accepting, maintenance, active_uid, is_member)) = row else {
        return Err(not_found());
    };
    if armed != 1 || accepting != 1 || maintenance != 0 {
        return Err(not_found());
    }

    let principal = active_uid;
    // A venue nobody owns is the guest namespace's, and signed out that is
    // whose namespace this is.
    let guest_venue = owner_uid.is_none();
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
        attributed: false,
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
        VenueResource::Folder(id) => ("SELECT venue_id FROM folders WHERE id = ?", id),
        VenueResource::GroupMember(id) => (
            "SELECT groups.venue_id
             FROM fixture_group_members member
             JOIN fixture_groups groups ON groups.id = member.group_id
             WHERE member.id = ?",
            id,
        ),
        VenueResource::Score(id) => ("SELECT venue_id FROM scores WHERE id = ?", id),
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

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    use super::*;

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
                VALUES ('shared-member', 'alice', 'shared-fixture', 'shared-group')",
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
            "INSERT INTO venue_members (id, uid, venue_id, role)
             VALUES ('shared:bob', 'bob', 'shared', 'member')",
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
    async fn members_edit_the_venue_while_only_owners_and_guests_hold_its_door() {
        let (_directory, pool) = test_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        VenueAccess::<Write>::write(&pool, VenueResource::Fixture("alice-fixture"))
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
        for resource in [
            VenueResource::Venue("shared"),
            VenueResource::Fixture("shared-fixture"),
            VenueResource::Group("shared-group"),
            VenueResource::GroupMember("shared-member"),
        ] {
            VenueAccess::<Write>::write(&pool, resource)
                .await
                .unwrap_or_else(|error| panic!("a member could not edit {resource:?}: {error}"))
                .commit()
                .await
                .unwrap();
        }
        assert!(
            VenueAccess::<Owner>::owner(&pool, VenueResource::Venue("shared"))
                .await
                .is_err(),
            "a member was admitted to manage who has the venue"
        );
        assert!(
            VenueAccess::<Write>::write(&pool, VenueResource::Venue("alice"))
                .await
                .is_err(),
            "a member of one venue could edit another the owner has"
        );
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
            VenueAccess::<Owner>::owner(&pool, VenueResource::Venue("guest"))
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
            VenueAccess::<Write>::write(&pool, VenueResource::Group("shared-group"))
                .await
                .is_ok()
        );
        assert!(
            VenueAccess::<Owner>::owner(&pool, VenueResource::Venue("shared"))
                .await
                .is_ok()
        );
        sqlx::query("UPDATE fixtures SET label = 'owner edit' WHERE id = 'shared-fixture'")
            .execute(&pool)
            .await
            .unwrap();
    }
}
