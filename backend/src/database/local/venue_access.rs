use std::marker::PhantomData;

use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

pub use crate::models::venues::VenueCapability;
use crate::sync::triggers::Attribution;

pub struct Read;
/// A persistent edit of the venue's content: its fixtures, groups, stage,
/// scores, environment and the rest. The owner and every member may make one;
/// the server's `public.history` records who did.
pub struct Write;
/// A change to who has the venue — its share code and its memberships. The
/// signed-in owner only.
pub struct Owner;

/// Who the admitted principal is to one venue, and so what they may do with
/// it: one row of the `auth_venue_access` view.
///
/// That view is the whole rule and the one place it is decided (see its
/// migration, `20261109120000_one_venue_standing.sql`): every guard here admits
/// by it, every listing and trigger that asks whether a venue is reachable
/// joins it, and a venue read carries its answer
/// ([`crate::models::venues::Venue::capability`]). Nothing compares a row's
/// `uid` with the principal to decide an edit. A venue the principal cannot
/// open has no row, so no `Standing`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Standing {
    /// The principal's own venue.
    pub owner: bool,
    pub capability: VenueCapability,
}

impl Standing {
    /// Read off a row that selected the view's `owner`, `edit` and `manage`.
    pub(crate) fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, String> {
        use sqlx::Row;
        let get = |column: &str| {
            row.try_get::<bool, _>(column)
                .map_err(|error| format!("Failed to read the venue's standing: {error}"))
        };
        Ok(Self {
            owner: get("owner")?,
            capability: VenueCapability {
                edit: get("edit")?,
                manage: get("manage")?,
            },
        })
    }
}

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

/// Opaque, transaction-bound proof that the app database admitted the
/// signed-in principal to one venue. Nobody signed in is refused with
/// [`SIGN_IN_REQUIRED`](crate::database::local::auth::SIGN_IN_REQUIRED): a
/// venue has an owner, and a venue nobody owns opens for no one. Mutations
/// use an IMMEDIATE transaction so the proof and write serialize with identity
/// switches; reads keep one deferred snapshot and may finish if they were
/// admitted before a switch began.
pub struct VenueAccess<'a, Mode> {
    transaction: Transaction<'a, Sqlite>,
    venue_id: String,
    principal: String,
    standing: Standing,
    /// Whether the writes carry an attribution, and so must be given back to
    /// the person before commit. See [`crate::sync::triggers::attribute`].
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
    fn principal(&self) -> &str;
    fn standing(&self) -> Standing;
    fn connection(&mut self) -> &mut SqliteConnection;
}

impl<Mode> sealed::Sealed for VenueAccess<'_, Mode> {}

impl<Mode> AuthorizedVenue for VenueAccess<'_, Mode> {
    fn venue_id(&self) -> &str {
        &self.venue_id
    }

    fn principal(&self) -> &str {
        &self.principal
    }

    fn standing(&self) -> Standing {
        self.standing
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
        authorize(transaction, resource, |_| true).await
    }
}

impl<'a> VenueAccess<'a, Write> {
    pub async fn write(pool: &'a SqlitePool, resource: VenueResource<'_>) -> Result<Self, String> {
        Self::write_as(pool, resource, Attribution::PERSON).await
    }

    /// A mutation made on someone else's behalf — a model's turn in a
    /// conversation, or an MCP client's session — so the server's history
    /// names it, and what it belongs to, instead of the person alone.
    /// [`Attribution::PERSON`] is the person, as [`Self::write`].
    pub async fn write_as(
        pool: &'a SqlitePool,
        resource: VenueResource<'_>,
        attribution: Attribution<'_>,
    ) -> Result<Self, String> {
        let transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| format!("Failed to begin venue mutation: {error}"))?;
        let mut access: Self = authorize(transaction, resource, |can| can.edit).await?;
        if attribution != Attribution::PERSON {
            crate::sync::triggers::attribute(&mut access.transaction, attribution).await?;
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
    /// Admit whoever may [`manage`](VenueCapability::manage) the venue — its
    /// signed-in owner, and nobody else — to change who has it.
    pub async fn owner(pool: &'a SqlitePool, resource: VenueResource<'_>) -> Result<Self, String> {
        let transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| format!("Failed to begin venue mutation: {error}"))?;
        authorize(transaction, resource, |can| can.manage).await
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

    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// What the admitted principal may do with this venue.
    pub fn capability(&self) -> VenueCapability {
        self.standing.capability
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
    admits: fn(VenueCapability) -> bool,
) -> Result<VenueAccess<'a, Mode>, String> {
    let principal = crate::database::local::auth::signed_in_principal(&mut transaction)
        .await?
        .ok_or_else(not_found)?;
    let venue_id = resolve_venue_id(&mut transaction, resource)
        .await?
        .ok_or_else(not_found)?;

    let row = sqlx::query(
        "SELECT owner, edit, manage FROM auth_venue_access
         WHERE venue_id = ? AND principal = ?",
    )
    .bind(&venue_id)
    .bind(&principal)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| format!("Failed to authorize venue access: {error}"))?
    .ok_or_else(not_found)?;
    let standing = Standing::from_row(&row)?;
    if !admits(standing.capability) {
        return Err(not_found());
    }

    Ok(VenueAccess {
        transaction,
        venue_id,
        principal,
        standing,
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
        // A venue from before sign-in was required: nobody owns it.
        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .unwrap();
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('unowned', NULL, 'Unowned')")
            .execute(&pool)
            .await
            .unwrap();
        (directory, pool)
    }

    /// What an agent's turn writes is queued under the turn: who, in which
    /// conversation, started by which message, and grouped with the rest of
    /// that turn. The pool has one connection, so the person's next write
    /// is on the one the agent just used, and carries none of it — whether
    /// the agent's write committed or was abandoned.
    #[tokio::test]
    async fn an_agents_write_names_its_turn_and_the_next_write_is_the_persons() {
        use crate::sync::triggers::{installed, TurnAttribution};

        let (_directory, pool) = test_pool().await;
        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        installed::on_the_only_connection(&pool).await;
        let relabel = |label: &'static str| {
            sqlx::query("UPDATE fixtures SET label = ? WHERE id = 'alice-fixture'").bind(label)
        };
        let fixture = VenueResource::Fixture("alice-fixture");

        let turn = TurnAttribution::turn(Some("claude-opus-5-5"), "thread-1", Some("message-1"));
        let mut agent = VenueAccess::<Write>::write_as(&pool, fixture, turn.get())
            .await
            .unwrap();
        relabel("agent").execute(agent.connection()).await.unwrap();
        agent.commit().await.unwrap();

        let mut abandoned = VenueAccess::<Write>::write_as(&pool, fixture, turn.get())
            .await
            .unwrap();
        relabel("abandoned")
            .execute(abandoned.connection())
            .await
            .unwrap();
        drop(abandoned);

        let mut person = VenueAccess::<Write>::write(&pool, fixture).await.unwrap();
        relabel("person")
            .execute(person.connection())
            .await
            .unwrap();
        person.commit().await.unwrap();

        assert_eq!(
            installed::attributions(&pool, "fixtures").await,
            vec![
                (
                    "PATCH".to_owned(),
                    Some(serde_json::json!({
                        "actor": "claude-opus-5-5",
                        "thread": "thread-1",
                        "message": "message-1",
                        "group": "message-1",
                    }))
                ),
                ("PATCH".to_owned(), None),
            ]
        );
    }

    #[tokio::test]
    async fn members_edit_the_venue_while_only_its_signed_in_owner_holds_its_door() {
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
            VenueAccess::<Read>::read(&pool, VenueResource::Venue("unowned"))
                .await
                .is_err()
        );

        crate::database::local::auth::arm_write_admission(&pool, None)
            .await
            .unwrap();
        // Signed out, nothing opens — an account's venue, a venue nobody
        // owns, or one that does not exist — and the refusal says to sign in.
        for venue in ["unowned", "alice", "shared", "missing"] {
            let resource = VenueResource::Venue(venue);
            let refusals = [
                VenueAccess::<Read>::read(&pool, resource).await.err(),
                VenueAccess::<Write>::write(&pool, resource).await.err(),
                VenueAccess::<Owner>::owner(&pool, resource).await.err(),
            ];
            for refusal in refusals {
                assert_eq!(
                    refusal.as_deref(),
                    Some(crate::database::local::auth::SIGN_IN_REQUIRED),
                    "a signed-out caller reached venue {venue}"
                );
            }
        }
        assert!(
            VenueAccess::<Read>::read(&pool, VenueResource::Fixture("alice-fixture"),)
                .await
                .is_err()
        );

        crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
            .await
            .unwrap();
        assert!(
            VenueAccess::<Read>::read(&pool, VenueResource::Venue("unowned"))
                .await
                .is_err(),
            "a venue nobody owns opened for a signed-in account"
        );
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
