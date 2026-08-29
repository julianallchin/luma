//! Addressing, against the database.
//!
//! [`luma_scene::patch`] owns the rule and knows nothing about rows; this
//! module is the one place the rule meets `fixtures`. Three jobs, and nothing
//! else:
//!
//! - **derive** — [`plan`] solves the venue and asks the allocator where every
//!   fixture goes; [`auto_patch`] writes that answer down;
//! - **refuse** — [`set_address`] and [`admit`] are the only doors a typed
//!   address comes through, and both go through [`Occupancy`], so "is this
//!   address free" has exactly one implementation;
//! - **report** — [`universe_occupancy`] is the single source for any footprint
//!   strip, collisions included.
//!
//! # The invariant this buys
//!
//! An address in the database is always addressable: the writes here refuse a
//! footprint that leaves its universe, and
//! `migrations/20260830000000_patch_addressing.sql` refuses it again in a
//! trigger, so a second writer (a sync pull, a repair script) cannot get one
//! past. That is what lets `fixtures::engine` drop its truncation branch: the
//! condition it guarded is now unrepresentable rather than merely unlikely.

use std::path::Path;

use luma_scene::patch::{allocate, Address, Allocation, Fixture, Footprint, Occupancy};
use thiserror::Error;

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::models::fixtures::PatchedFixture;
use crate::models::patch::{AutoPatchReport, PatchNote, UniverseCell};

/// Why an address was refused.
///
/// Both variants are *statements about the patch*, not about the call: they
/// name the universe, the address and — for a collision — the fixture already
/// there, so the page can put the cursor on the row that is in the way.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PatchError {
    /// The footprint runs off the end of a universe, or starts before channel 1.
    #[error("address {address} with {channels} channels does not fit in universe {universe}: DMX addresses run 1 to 512")]
    OutOfRange {
        universe: u16,
        address: u16,
        channels: u16,
    },

    /// Somebody is already there.
    #[error("universe {universe} address {address} collides with {conflict}")]
    Collision {
        universe: u16,
        address: u16,
        conflict: String,
    },

    /// Anything the database said no to.
    #[error("{0}")]
    Database(String),
}

impl From<String> for PatchError {
    fn from(message: String) -> Self {
        PatchError::Database(message)
    }
}

// ---------------------------------------------------------------------------
// Deriving
// ---------------------------------------------------------------------------

/// The patch rows as the allocator's input.
///
/// A channel count wider than a universe cannot exist: the
/// `fixtures_address_fits_universe_*` triggers
/// (`migrations/20260830000000_patch_addressing.sql`) test
/// `address + num_channels - 1 > 512` with `address >= 1`, which no width over
/// 512 can pass at any address, and
/// `migrations/20260831000000_patch_width_repair.sql` clamped the rows written
/// before them. So the conversion here is total, not a decision.
///
/// The row's *stored* footprint is carried across whether or not it is pinned:
/// it is what a new address has to avoid colliding with, and dropping it here
/// is what made `next_addresses` and [`admit`] disagree about which channels
/// are free.
fn inputs(rows: &[PatchedFixture]) -> Vec<Fixture> {
    rows.iter()
        .map(|row| {
            let channels = u16::try_from(row.num_channels).unwrap_or(u16::MAX);
            let address = match (footprint_of(row), row.address_pinned) {
                (Some(footprint), true) => Address::Pinned(footprint),
                (Some(footprint), false) => Address::Derived(footprint),
                (None, _) => Address::Unset,
            };
            Fixture {
                id: row.id.clone(),
                channels,
                address,
            }
        })
        .collect()
}

/// One row's stored footprint. `None` only for a row written before the
/// migration's repair pass, which cannot happen after it.
fn footprint_of(row: &PatchedFixture) -> Option<Footprint> {
    Footprint::new(
        u16::try_from(row.universe).ok()?,
        u16::try_from(row.address).ok()?,
        u16::try_from(row.num_channels).ok()?,
    )
}

/// Where every fixture in the venue belongs, without writing anything.
///
/// # Errors
/// Fails if the patch cannot be read or the venue cannot be solved.
pub async fn plan(
    access: &mut impl AuthorizedVenue,
    fixtures_root: &Path,
) -> Result<Allocation, String> {
    let rows = fixtures_db::get_patched_fixtures(access).await?;
    let venue = crate::venue_graph::resolved(access, fixtures_root).await?;
    Ok(allocate(&venue, &inputs(&rows)))
}

/// Re-derive every address from placement and write the result.
///
/// **Discards hand-set addresses.** Auto Patch is a deliberate "redo the
/// paperwork", so the overrides it re-derives over are cleared rather than
/// stepped around (`docs/specs/venue-builder-gauntlet.md` §3). Every other
/// path — a new fixture, a distribution, [`next_addresses`] — goes through
/// [`allocate`], which preserves them.
///
/// # Errors
/// Fails if the venue cannot be solved or a write is refused.
pub async fn auto_patch(
    access: &mut VenueAccess<'_, Write>,
    fixtures_root: &Path,
) -> Result<AutoPatchReport, String> {
    let discarded = fixtures_db::clear_address_pins(access).await?;
    let allocation = plan(access, fixtures_root).await?;
    let before: Vec<PatchedFixture> = fixtures_db::get_patched_fixtures(access).await?;

    let mut moved = 0;
    for assignment in &allocation.assignments {
        let universe = i64::from(assignment.footprint.universe());
        let address = i64::from(assignment.footprint.address());
        let unchanged = before.iter().any(|row| {
            row.id == assignment.fixture && row.universe == universe && row.address == address
        });
        if unchanged {
            continue;
        }
        fixtures_db::update_fixture_address(access, &assignment.fixture, universe, address, false)
            .await?;
        moved += 1;
    }

    Ok(AutoPatchReport {
        moved,
        overrides_discarded: usize::try_from(discarded).unwrap_or(usize::MAX),
        notes: allocation.notes.iter().map(PatchNote::from).collect(),
    })
}

/// Where the next `count` fixtures of `channels` channels each would go if they
/// were added to `run` now — what a distribution needs before its fixtures
/// exist. `run` of `None` asks for tray addresses.
///
/// # Errors
/// Fails if the patch cannot be read or the venue cannot be solved.
pub async fn next_addresses(
    access: &mut impl AuthorizedVenue,
    fixtures_root: &Path,
    run: Option<&str>,
    channels: u16,
    count: usize,
) -> Result<Vec<Footprint>, String> {
    let rows = fixtures_db::get_patched_fixtures(access).await?;
    let venue = crate::venue_graph::resolved(access, fixtures_root).await?;
    Ok(luma_scene::patch::next_addresses(
        &venue,
        &inputs(&rows),
        run,
        channels,
        count,
    ))
}

// ---------------------------------------------------------------------------
// Refusing
// ---------------------------------------------------------------------------

/// What is patched where, read straight from the rows.
///
/// Built from the database rather than from an allocation on purpose: a
/// collision is a property of what is *stored*, and asking the allocator would
/// only ever describe a patch nobody has written yet.
///
/// # Errors
/// Fails if the patch cannot be read.
pub async fn occupancy(access: &mut impl AuthorizedVenue) -> Result<Occupancy, String> {
    let rows = fixtures_db::get_patched_fixtures(access).await?;
    Ok(Occupancy::of(rows.iter().filter_map(|row| {
        footprint_of(row).map(|f| (f, row.id.clone()))
    })))
}

/// Admit an address, or say why not.
///
/// `mover` is the fixture being addressed, so re-addressing a fixture to where
/// it already is does not collide with itself. `None` for a fixture that does
/// not exist yet.
///
/// # Errors
/// [`PatchError::OutOfRange`] if the footprint leaves the universe,
/// [`PatchError::Collision`] if something is already there.
pub fn admit(
    occupancy: &Occupancy,
    mover: Option<&str>,
    universe: u16,
    address: u16,
    channels: u16,
) -> Result<Footprint, PatchError> {
    let footprint = Footprint::new(universe, address, channels).ok_or(PatchError::OutOfRange {
        universe,
        address,
        channels,
    })?;
    let mut free = occupancy.clone();
    if let Some(mover) = mover {
        free.release(mover);
    }
    match free.conflict(&footprint) {
        Some(conflict) => Err(PatchError::Collision {
            universe,
            address,
            conflict: conflict.to_string(),
        }),
        None => Ok(footprint),
    }
}

/// Set one fixture's address by hand, and pin it there.
///
/// Refuses rather than truncates, and the refusal happens before any write, so
/// a rejected edit leaves the database exactly as it was.
///
/// # Errors
/// As [`admit`], or if the fixture is not in this venue.
pub async fn set_address(
    access: &mut VenueAccess<'_, Write>,
    id: &str,
    universe: u16,
    address: u16,
) -> Result<(), PatchError> {
    let rows = fixtures_db::get_patched_fixtures(access).await?;
    let row = rows
        .iter()
        .find(|row| row.id == id)
        .ok_or_else(|| PatchError::Database(format!("no fixture {id} in this venue")))?;
    let channels = u16::try_from(row.num_channels).unwrap_or(u16::MAX);

    let occupancy = Occupancy::of(
        rows.iter()
            .filter_map(|row| footprint_of(row).map(|f| (f, row.id.clone()))),
    );
    let footprint = admit(&occupancy, Some(id), universe, address, channels)?;

    fixtures_db::update_fixture_address(
        access,
        id,
        i64::from(footprint.universe()),
        i64::from(footprint.address()),
        true,
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

/// One universe as 512 cells — the single source for any footprint grid.
///
/// # Errors
/// Fails if the patch cannot be read.
pub async fn universe_occupancy(
    access: &mut impl AuthorizedVenue,
    universe: u16,
) -> Result<Vec<UniverseCell>, String> {
    let rows = fixtures_db::get_patched_fixtures(access).await?;
    let occupancy = Occupancy::of(
        rows.iter()
            .filter_map(|row| footprint_of(row).map(|f| (f, row.id.clone()))),
    );
    Ok(occupancy
        .cells(universe)
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            #[allow(clippy::cast_possible_truncation)]
            UniverseCell::new(index as u16 + 1, cell, |id| {
                rows.iter()
                    .find(|row| row.id == id)
                    .map_or((None, false), |row| (row.label.clone(), row.address_pinned))
            })
        })
        .collect())
}

/// Every universe anything is patched into, ascending — what a page needs to
/// know which strips to draw.
///
/// # Errors
/// Fails if the patch cannot be read.
pub async fn universes_in_use(access: &mut impl AuthorizedVenue) -> Result<Vec<u16>, String> {
    Ok(occupancy(access).await?.universes().collect())
}

#[cfg(test)]
mod tests;
