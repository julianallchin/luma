//! The rule, against a real database and the real catalog.
//!
//! `luma_scene::patch`'s own tests pin the rule over a stub socket table; these
//! pin the *chain* — rows to graph to solve to allocation to rows again —
//! because that is where the pieces are wired together and where a wrong
//! column or a lost pin would actually show up.

use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

use super::*;
use crate::database::local::venue_access::{Read, VenueResource};

const VENUE: &str = "venue-under-test";

/// Where the meshes are. `stage_render::meshes_root` falls back to the repo's
/// `resources/meshes` when this has no sibling of its own, which is what makes
/// a unit test able to resolve the real catalog.
fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../resources/fixtures")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("patch.db");
    // Migrations run with foreign keys off — `20260323000000` rebuilds tables
    // in an order that a live FK graph would refuse — then the real pool turns
    // them back on.
    let migrating = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .expect("migration pool");
    sqlx::migrate!("./migrations")
        .run(&migrating)
        .await
        .expect("migrations");
    migrating.close().await;
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .expect("pool");
    crate::database::local::auth::arm_write_admission(&pool, Some("alice"))
        .await
        .expect("arm alice");
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES (?, 'alice', 'Patch test')")
        .bind(VENUE)
        .execute(&pool)
        .await
        .expect("venue");
    (directory, pool)
}

/// One fixture: a patch row, a graph node, and the edge that hangs it under a
/// run at `along` metres.
///
/// The catalog's straight truss carries one host socket at its origin whose
/// tangent runs along the span (`luma_render::catalog::procedural_sockets`), so
/// `u` *is* `along(t)` in metres, which is the number the rule sorts by.
struct Hung {
    id: &'static str,
    run: &'static str,
    along: f64,
    channels: i64,
    pinned: Option<(i64, i64)>,
}

/// Two runs, eight movers on one and six bars on the other, hung in an order
/// that has nothing to do with where they end up — plus one bar whose address
/// the rental company already printed.
fn rig() -> Vec<Hung> {
    let mut rig = Vec::new();
    for (index, along) in [3.5, 0.5, 2.5, 1.5, 3.0, 1.0, 2.0, 0.0]
        .into_iter()
        .enumerate()
    {
        rig.push(Hung {
            id: MOVERS[index],
            run: "run-downstage",
            along,
            channels: 16,
            pinned: None,
        });
    }
    for (index, along) in [1.2, 0.0, 3.6, 2.4, 4.8, 6.0].into_iter().enumerate() {
        rig.push(Hung {
            id: BARS[index],
            run: "run-upstage",
            along,
            channels: 9,
            pinned: (index == 2).then_some((2, 300)),
        });
    }
    rig
}

const MOVERS: [&str; 8] = [
    "mover-0", "mover-1", "mover-2", "mover-3", "mover-4", "mover-5", "mover-6", "mover-7",
];
const BARS: [&str; 6] = ["bar-0", "bar-1", "bar-2", "bar-3", "bar-4", "bar-5"];

async fn seed(pool: &SqlitePool) {
    let root = format!("{VENUE}:venue");
    sqlx::query(
        "INSERT INTO venue_nodes (id, uid, venue_id, kind) VALUES (?, 'alice', ?, 'venue')",
    )
    .bind(&root)
    .bind(VENUE)
    .execute(pool)
    .await
    .expect("root");

    for run in ["run-downstage", "run-upstage"] {
        sqlx::query(
            "INSERT INTO venue_nodes (id, uid, venue_id, kind, catalog_ref, label)
             VALUES (?, 'alice', ?, 'run', 'truss/straight', ?)",
        )
        .bind(run)
        .bind(VENUE)
        .bind(run)
        .execute(pool)
        .await
        .expect("run node");
        sqlx::query(
            "INSERT INTO venue_edges (child_id, parent_id, my_socket, their_socket, roll)
             VALUES (?, ?, 'grab', 'floor', 0.0)",
        )
        .bind(run)
        .bind(&root)
        .execute(pool)
        .await
        .expect("run edge");
        sqlx::query("INSERT INTO venue_node_params (node_id, key, value) VALUES (?, 'span', 8.0)")
            .bind(run)
            .execute(pool)
            .await
            .expect("span");
    }

    for fixture in rig() {
        let (universe, address) = fixture.pinned.unwrap_or((1, 1));
        sqlx::query(
            "INSERT INTO fixtures
                (id, uid, venue_id, universe, address, num_channels, manufacturer, model,
                 mode_name, fixture_path, label, address_pinned)
             VALUES (?, 'alice', ?, ?, ?, ?, 'Test', 'Fixture', 'Mode', 'test.qxf', ?, ?)",
        )
        .bind(fixture.id)
        .bind(VENUE)
        .bind(universe)
        .bind(address)
        .bind(fixture.channels)
        .bind(fixture.id)
        .bind(i64::from(fixture.pinned.is_some()))
        .execute(pool)
        .await
        .expect("fixture row");

        sqlx::query(
            "INSERT INTO venue_nodes (id, uid, venue_id, kind, catalog_ref, label)
             VALUES (?, 'alice', ?, 'fixture', ?, ?)",
        )
        .bind(fixture.id)
        .bind(VENUE)
        .bind(fixture.id)
        .bind(fixture.id)
        .execute(pool)
        .await
        .expect("fixture node");
        sqlx::query(
            "INSERT INTO venue_edges (child_id, parent_id, my_socket, their_socket, roll)
             VALUES (?, ?, 'clamp', 'grab', 0.0)",
        )
        .bind(fixture.id)
        .bind(fixture.run)
        .execute(pool)
        .await
        .expect("fixture edge");
        sqlx::query("INSERT INTO venue_node_params (node_id, key, value) VALUES (?, 'u', ?)")
            .bind(fixture.id)
            .bind(fixture.along)
            .execute(pool)
            .await
            .expect("u");
    }
}

async fn seeded() -> (tempfile::TempDir, SqlitePool) {
    let (directory, pool) = test_pool().await;
    seed(&pool).await;
    (directory, pool)
}

async fn read(pool: &SqlitePool) -> VenueAccess<'_, Read> {
    VenueAccess::<Read>::read(pool, VenueResource::Venue(VENUE))
        .await
        .expect("read scope")
}

async fn write(pool: &SqlitePool) -> VenueAccess<'_, Write> {
    VenueAccess::<Write>::write(pool, VenueResource::Venue(VENUE))
        .await
        .expect("write scope")
}

/// The patch as the database holds it: `(id, universe, address, pinned)`.
async fn stored(pool: &SqlitePool) -> Vec<(String, i64, i64, bool)> {
    let mut access = read(pool).await;
    let mut rows: Vec<(String, i64, i64, bool)> = fixtures_db::get_patched_fixtures(&mut access)
        .await
        .expect("patch")
        .into_iter()
        .map(|row| (row.id, row.universe, row.address, row.address_pinned))
        .collect();
    rows.sort();
    rows
}

// ---------------------------------------------------------------------------
// The golden
// ---------------------------------------------------------------------------

#[tokio::test]
async fn seeded_venue_patch_golden_is_current() {
    let (_dir, pool) = seeded().await;
    let mut access = read(&pool).await;
    let allocation = plan(&mut access, &fixtures_root()).await.expect("plan");

    let rows: Vec<serde_json::Value> = allocation
        .assignments
        .iter()
        .map(|a| {
            serde_json::json!({
                "fixture": a.fixture,
                "run": a.run,
                "universe": a.footprint.universe(),
                "address": a.footprint.address(),
                "footprint": [a.footprint.address(), a.footprint.last()],
                "pinned": a.pinned,
            })
        })
        .collect();
    let mut contents = serde_json::to_string_pretty(&serde_json::json!({
        "patch": rows,
        "notes": allocation
            .notes
            .iter()
            .map(|n| crate::models::patch::PatchNote::from(n).message)
            .collect::<Vec<_>>(),
    }))
    .expect("serializes");
    contents.push('\n');

    let path = repo_root().join("harness/goldens/patch/seeded-venue.json");
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("golden directory");
    let same = std::fs::read_to_string(&path).is_ok_and(|old| old == contents);
    if !same {
        std::fs::write(&path, &contents).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
    assert!(
        same,
        "the seeded-venue patch golden was stale and has been rewritten — review and commit it"
    );
}

// ---------------------------------------------------------------------------
// The chain
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auto_patch_writes_physical_order_through_to_the_rows() {
    let (_dir, pool) = seeded().await;
    let before = stored(&pool).await;

    let mut access = write(&pool).await;
    let report = auto_patch(&mut access, &fixtures_root())
        .await
        .expect("auto");
    access.commit().await.expect("commit");

    let after = stored(&pool).await;
    assert_ne!(before, after, "auto-patch changed nothing at all");
    assert!(report.moved > 0);
    assert_eq!(
        report.overrides_discarded, 1,
        "the one hand-set address was the one override"
    );
    assert!(
        after.iter().all(|(_, _, _, pinned)| !pinned),
        "auto-patch left a pin behind"
    );

    // The movers were hung back-to-front; the addresses they ended up with run
    // in the order they hang, not the order they were added.
    let hung: Vec<&'static str> = {
        let mut downstage: Vec<Hung> = rig()
            .into_iter()
            .filter(|f| f.run == "run-downstage")
            .collect();
        downstage.sort_by(|a, b| a.along.partial_cmp(&b.along).expect("finite"));
        downstage.iter().map(|f| f.id).collect()
    };
    let mut addressed: Vec<(i64, &str)> = after
        .iter()
        .filter(|(id, ..)| hung.contains(&id.as_str()))
        .map(|(id, universe, address, _)| (*universe * 1024 + *address, id.as_str()))
        .collect();
    addressed.sort();
    let by_address: Vec<&str> = addressed.iter().map(|(_, id)| *id).collect();
    assert_eq!(by_address, hung);
}

#[tokio::test]
async fn a_pinned_address_survives_an_allocation_and_the_rest_flow_around_it() {
    let (_dir, pool) = seeded().await;
    let mut access = read(&pool).await;
    let allocation = plan(&mut access, &fixtures_root()).await.expect("plan");

    let pinned = allocation.get("bar-2").expect("bar-2 allocated");
    assert!(pinned.pinned);
    assert_eq!(
        (pinned.footprint.universe(), pinned.footprint.address()),
        (2, 300),
        "the printed address moved"
    );
    for other in &allocation.assignments {
        if other.fixture != "bar-2" {
            assert!(!other.footprint.overlaps(&pinned.footprint));
        }
    }
}

#[tokio::test]
async fn each_run_lands_in_its_own_universe() {
    let (_dir, pool) = seeded().await;
    let mut access = read(&pool).await;
    let allocation = plan(&mut access, &fixtures_root()).await.expect("plan");

    let universe_of = |fixture: &str| {
        allocation
            .get(fixture)
            .expect("allocated")
            .footprint
            .universe()
    };
    let movers: std::collections::BTreeSet<u16> = MOVERS.iter().map(|id| universe_of(id)).collect();
    let bars: std::collections::BTreeSet<u16> = BARS
        .iter()
        .filter(|id| **id != "bar-2")
        .map(|id| universe_of(id))
        .collect();
    assert_eq!(movers.len(), 1);
    assert_eq!(bars.len(), 1);
    assert!(movers.is_disjoint(&bars));
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_colliding_address_is_refused_and_the_row_does_not_move() {
    let (_dir, pool) = seeded().await;
    {
        let mut access = write(&pool).await;
        auto_patch(&mut access, &fixtures_root())
            .await
            .expect("auto");
        access.commit().await.expect("commit");
    }
    let before = stored(&pool).await;
    let victim = before
        .iter()
        .find(|(id, ..)| id == "mover-0")
        .cloned()
        .expect("mover-0");
    let onto = before
        .iter()
        .find(|(id, universe, ..)| id != "mover-0" && *universe == victim.1)
        .cloned()
        .expect("a neighbour in the same universe");

    let mut access = write(&pool).await;
    let refused = set_address(
        &mut access,
        "mover-0",
        u16::try_from(onto.1).expect("universe"),
        u16::try_from(onto.2).expect("address"),
    )
    .await;
    access.commit().await.expect("commit");

    match refused {
        Err(PatchError::Collision { conflict, .. }) => assert_eq!(conflict, onto.0),
        other => panic!("expected a named collision, got {other:?}"),
    }
    assert_eq!(
        stored(&pool).await,
        before,
        "a refused edit wrote something"
    );
}

#[tokio::test]
async fn an_address_whose_footprint_leaves_the_universe_is_refused() {
    let (_dir, pool) = seeded().await;
    let before = stored(&pool).await;

    let mut access = write(&pool).await;
    // mover-0 is 16 channels wide, so it cannot start on the last channel.
    let refused = set_address(&mut access, "mover-0", 1, luma_scene::patch::UNIVERSE_SIZE).await;
    access.commit().await.expect("commit");

    assert!(
        matches!(refused, Err(PatchError::OutOfRange { .. })),
        "expected a range refusal, got {refused:?}"
    );
    assert_eq!(stored(&pool).await, before);
}

#[tokio::test]
async fn the_database_refuses_an_out_of_range_footprint_even_without_the_service() {
    // The invariant is the table's, not the service's: this is what makes the
    // DMX engine's truncation branch unreachable rather than merely unused.
    let (_dir, pool) = seeded().await;
    // A legal move first, so the refusal below is the range check answering and
    // not write admission refusing every raw statement.
    sqlx::query("UPDATE fixtures SET address = 400 WHERE id = 'mover-0'")
        .execute(&pool)
        .await
        .expect("400 + 16 - 1 = 415 fits");
    let refused = sqlx::query("UPDATE fixtures SET address = 500 WHERE id = 'mover-0'")
        .execute(&pool)
        .await;
    assert!(
        refused.is_err(),
        "500 + 16 - 1 = 515 was written to the patch"
    );
}

#[tokio::test]
async fn setting_an_address_by_hand_pins_it() {
    let (_dir, pool) = seeded().await;
    {
        let mut access = write(&pool).await;
        auto_patch(&mut access, &fixtures_root())
            .await
            .expect("auto");
        access.commit().await.expect("commit");
    }
    let mut access = write(&pool).await;
    set_address(&mut access, "mover-0", 9, 100)
        .await
        .expect("a free address in an empty universe");
    access.commit().await.expect("commit");

    let row = stored(&pool)
        .await
        .into_iter()
        .find(|(id, ..)| id == "mover-0")
        .expect("mover-0");
    assert_eq!((row.1, row.2, row.3), (9, 100, true));

    // And the next allocation leaves it there.
    let mut access = read(&pool).await;
    let allocation = plan(&mut access, &fixtures_root()).await.expect("plan");
    let assignment = allocation.get("mover-0").expect("allocated");
    assert!(assignment.pinned);
    assert_eq!(
        (
            assignment.footprint.universe(),
            assignment.footprint.address()
        ),
        (9, 100)
    );
}

// ---------------------------------------------------------------------------
// The strip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn universe_occupancy_is_the_footprint_of_what_is_stored() {
    let (_dir, pool) = seeded().await;
    {
        let mut access = write(&pool).await;
        auto_patch(&mut access, &fixtures_root())
            .await
            .expect("auto");
        access.commit().await.expect("commit");
    }
    let stored = stored(&pool).await;
    let (id, universe, address, _) = stored
        .iter()
        .find(|(id, ..)| id == "mover-0")
        .cloned()
        .expect("mover-0");

    let mut access = read(&pool).await;
    let cells = universe_occupancy(&mut access, u16::try_from(universe).expect("universe"))
        .await
        .expect("cells");

    assert_eq!(cells.len(), usize::from(luma_scene::patch::UNIVERSE_SIZE));
    let first = &cells[usize::try_from(address).expect("address") - 1];
    assert_eq!(first.fixture_id.as_deref(), Some(id.as_str()));
    assert_eq!(first.channel, 0);
    assert_eq!(first.label.as_deref(), Some("mover-0"));
    assert!(!first.collision);
    // Sixteen channels wide, and the seventeenth belongs to somebody else.
    let occupied = cells
        .iter()
        .filter(|cell| cell.fixture_id.as_deref() == Some(id.as_str()))
        .count();
    assert_eq!(occupied, 16);
    assert!(cells.iter().all(|cell| !cell.collision));
}

#[tokio::test]
async fn universes_in_use_names_every_universe_the_patch_touches() {
    let (_dir, pool) = seeded().await;
    let mut access = read(&pool).await;
    let universes = universes_in_use(&mut access).await.expect("universes");
    let stored: std::collections::BTreeSet<u16> = stored(&pool)
        .await
        .iter()
        .map(|(_, universe, ..)| u16::try_from(*universe).expect("universe"))
        .collect();
    assert_eq!(universes, stored.into_iter().collect::<Vec<_>>());
}

// ---------------------------------------------------------------------------
// What a distribution asks for
// ---------------------------------------------------------------------------

/// Every slot the allocator offers has to survive the door it will be carried
/// through.
///
/// The rig here is packed sequentially into universe 1 — what one-at-a-time
/// adds write, and what a venue looks like until somebody presses Auto Patch —
/// so the stored patch and the derived one genuinely disagree. An offer
/// computed against the derivation alone lands in a hole the rule left in
/// universe 1 that a row is sitting in, and [`admit`] refuses it.
#[tokio::test]
async fn every_offered_slot_is_one_admit_accepts_even_before_an_auto_patch() {
    let (_dir, pool) = seeded().await;
    {
        // Repack: consecutive in universe 1, in creation order, nothing pinned.
        let mut address = 1i64;
        let mut access = write(&pool).await;
        for row in fixtures_db::get_patched_fixtures(&mut access)
            .await
            .expect("patch")
        {
            fixtures_db::update_fixture_address(&mut access, &row.id, 1, address, false)
                .await
                .expect("repack");
            address += row.num_channels;
        }
        access.commit().await.expect("commit");
    }

    let mut access = read(&pool).await;
    let occupancy = occupancy(&mut access).await.expect("occupancy");
    // The rule and the rows disagree, or this would prove nothing.
    let derived = plan(&mut access, &fixtures_root()).await.expect("plan");
    assert!(
        derived
            .assignments
            .iter()
            .any(|a| a.footprint.universe() != 1),
        "the rule spreads the runs over universes; the rows are all in one"
    );

    for run in [None, Some("run-downstage"), Some("run-upstage")] {
        let slots = next_addresses(&mut access, &fixtures_root(), run, 16, 3)
            .await
            .expect("slots");
        assert_eq!(slots.len(), 3);
        for slot in &slots {
            admit(
                &occupancy,
                None,
                slot.universe(),
                slot.address(),
                slot.channels(),
            )
            .unwrap_or_else(|error| {
                panic!("{run:?} was offered {slot:?}, which is refused: {error}")
            });
        }
    }
}

#[tokio::test]
async fn next_addresses_hands_out_slots_nothing_else_holds() {
    let (_dir, pool) = seeded().await;
    {
        let mut access = write(&pool).await;
        auto_patch(&mut access, &fixtures_root())
            .await
            .expect("auto");
        access.commit().await.expect("commit");
    }
    let mut access = read(&pool).await;
    let slots = next_addresses(&mut access, &fixtures_root(), Some("run-downstage"), 12, 4)
        .await
        .expect("slots");
    assert_eq!(slots.len(), 4);

    let occupancy = occupancy(&mut access).await.expect("occupancy");
    for slot in &slots {
        assert_eq!(occupancy.conflict(slot), None, "{slot:?} is already taken");
    }
    for pair in slots.windows(2) {
        assert!(!pair[0].overlaps(&pair[1]));
    }
}

/// A row written before anything validated, 600 channels wide, has to come out
/// of the migrations *writable*.
///
/// `20260830000000` clamped `num_channels` up to 1 and moved a bad `address`
/// to 1, but never clamped a width down — and its triggers fire on
/// `address + num_channels - 1 > 512`, which no address satisfies once the
/// width alone is over 512. So such a row survived that migration and then
/// refused every UPDATE, taking `auto_patch` (one transaction over the whole
/// venue) down with it for that venue, forever. `20260831000000` is the repair.
#[tokio::test]
async fn a_fixture_wider_than_a_universe_is_repaired_into_something_writable() {
    use sqlx::migrate::Migrate;

    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("wide.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .journal_mode(SqliteJournalMode::Wal)
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .expect("pool");

    // Applied one at a time so the row can be seeded at the one moment it was
    // ever writable: after `fixtures` exists, before the triggers do.
    let migrator = sqlx::migrate!("./migrations");
    let mut connection = pool.acquire().await.expect("connection");
    connection
        .ensure_migrations_table(&migrator.table_name)
        .await
        .expect("migrations table");
    for migration in migrator.iter() {
        if migration.migration_type.is_down_migration() {
            continue;
        }
        if migration.version == 20_260_830_000_000 {
            sqlx::query(
                "INSERT INTO venues (id, uid, name) VALUES ('v-wide', 'u-1', 'Room');
                 INSERT INTO fixtures
                   (id, uid, venue_id, universe, address, num_channels,
                    manufacturer, model, mode_name, fixture_path)
                 VALUES ('too-wide', 'u-1', 'v-wide', 1, 500, 600,
                    'Acme', 'Mover', '600ch', 'acme/mover.qxf')",
            )
            .execute(&mut *connection)
            .await
            .expect("seed the row the triggers would later refuse");
        }
        connection
            .apply(&migrator.table_name, migration)
            .await
            .expect("apply");
    }

    let (address, channels): (i64, i64) =
        sqlx::query_as("SELECT address, num_channels FROM fixtures WHERE id = 'too-wide'")
            .fetch_one(&mut *connection)
            .await
            .expect("the row is still there");
    assert_eq!(channels, 512, "a width no universe can hold is clamped");
    assert_eq!(
        address, 1,
        "and the footprint is moved somewhere addressable"
    );

    // The point of the clamp: the row can be written again. Before it, this
    // UPDATE aborted, and so did every auto-patch of the venue.
    sqlx::query("UPDATE fixtures SET address = 1, num_channels = 16 WHERE id = 'too-wide'")
        .execute(&mut *connection)
        .await
        .expect("the repaired row is updatable");
}
