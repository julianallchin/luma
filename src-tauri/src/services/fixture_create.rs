//! The one way a fixture comes into existence.
//!
//! A fixture is two rows: a `fixtures` row (the paperwork — definition, mode,
//! universe, address) and a `venue_nodes` row (the thing in the room). Before
//! this module they were written in different places at different times, and a
//! fixture added on the patch page got the first and not the second — so the
//! resolver never saw it, `unplaced` never listed it, and the tray it was
//! supposed to be sitting in was empty. "Patched but not placed" and "does not
//! exist" were the same state.
//!
//! [`create`] writes both, in the caller's transaction, and is the constructor
//! both doors use: the patch page's non-placed add and
//! [`crate::services::distribute`]. The gauntlet's AF9 — "a fixture exists that
//! no distribution and no patch-page add created" — is unrepresentable because
//! there is nowhere else to make one.
//!
//! [`delete`] is its dual, and for the same reason: the row and the node go
//! together or neither goes. Deleting only the paperwork left a node the
//! resolver kept posing — a light in the render that nothing in the patch
//! could name.
//!
//! # A new fixture has no edge, and that is the point
//!
//! [`create`] never writes a `venue_edges` row. A node with no edge is
//! **unplaced** ([`luma_scene::venue::ResolvedVenue::unplaced`]) — which is
//! exactly what a fixture in the tray is, and what lets the stage page's
//! tray→truss drag `reattach` it rather than invent a placement. A distribution
//! attaches its own edges afterwards; nothing here guesses one.
//!
//! # Naming
//!
//! [`ModelNumbering`] is the single rule: a label is `<term> <n>`, where `term`
//! is the model (or a distribution's `label_prefix`) and `n` is one past the
//! highest number any label in the venue already claims for that term. It lives
//! here rather than in the derivation because a name is minted when the fixture
//! is made: what a light is called is a fact about the row, not something a
//! solve recomputes. The frontend held a copy of this rule, spelled
//! `<model> (<n>)` — drifted, as duplicated rules do — and it has been
//! deleted, as has the derivation's.

use std::collections::BTreeMap;

use luma_scene::patch::Footprint;

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::venue_access::{VenueAccess, Write};
use crate::database::local::venue_graph as venue_graph_db;
use crate::models::fixtures::PatchedFixture;
use crate::services::patch::PatchError;

/// Running numbers per naming term, `<term> <n>`.
///
/// A counter rather than a function of the row list, because a batch writes
/// several rows before any of them are read back: each call consumes its
/// number, so eight movers distributed in one transaction number 1..8 instead
/// of all claiming 1.
#[derive(Debug, Default, Clone)]
pub struct ModelNumbering(BTreeMap<String, usize>);

impl ModelNumbering {
    /// Seeded by the labels a venue already carries.
    ///
    /// By the **highest number claimed**, not by how many rows there are:
    /// deleting `Aura 2` out of three must not make the next one `Aura 3`
    /// again. A label that is not `<term> <n>` — anything a human renamed —
    /// claims nothing, which is the right answer both ways round: it does not
    /// hold a number hostage, and nothing will be minted on top of it.
    pub fn seeded_by_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> ModelNumbering {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for label in labels {
            let Some((term, n)) = label.rsplit_once(' ') else {
                continue;
            };
            let Ok(n) = n.parse::<usize>() else {
                continue;
            };
            let claimed = counts.entry(term.to_string()).or_default();
            *claimed = (*claimed).max(n);
        }
        ModelNumbering(counts)
    }

    /// The next label for `term`, consuming its number.
    pub fn next(&mut self, term: &str) -> String {
        let n = self.0.entry(term.to_string()).or_default();
        *n += 1;
        format!("{term} {n}")
    }
}

/// A fixture to bring into existence. Everything here is a fact about the
/// paperwork; where it hangs is a separate write, and there may not be one.
#[derive(Debug, Clone)]
pub struct NewFixture<'a> {
    pub manufacturer: &'a str,
    pub model: &'a str,
    pub mode_name: &'a str,
    pub fixture_path: &'a str,
    /// Where it lands, already admitted against the patch by the caller — which
    /// is what makes this function unable to refuse anything.
    pub footprint: Footprint,
    /// Whether a human typed this address, in which case the allocator leaves
    /// it alone.
    pub pinned: bool,
    /// What to call it.
    pub name: Naming<'a>,
}

/// How a new fixture gets its label.
///
/// An enum rather than an explicit label beside an optional prefix, because
/// "given *and* minted" is not a thing anybody can mean, so it should not be a
/// pair anybody can construct.
#[derive(Debug, Clone)]
pub enum Naming<'a> {
    /// A name a caller chose. Used verbatim.
    Given(String),
    /// The venue's next `<term> <n>`, `term` defaulting to the model — which is
    /// a distribution's `label_prefix` when it has one.
    Minted(Option<&'a str>),
}

/// Write the patch row and the tray node, in the caller's transaction.
///
/// The node carries the **same id** as the row — the contract
/// [`luma_scene::patch::Fixture::id`] states and
/// [`crate::venue_graph::migrate`] establishes — so a placement is found by
/// looking up the fixture, with no join table between them.
///
/// # Errors
/// Only the database's: the address was admitted before the call, so there is
/// no decision left here to refuse.
pub async fn create(
    access: &mut VenueAccess<'_, Write>,
    numbering: &mut ModelNumbering,
    spec: NewFixture<'_>,
) -> Result<PatchedFixture, PatchError> {
    let label = match spec.name {
        Naming::Given(label) => label,
        Naming::Minted(term) => numbering.next(term.unwrap_or(spec.model)),
    };
    let fixture = fixtures_db::insert_fixture(
        access,
        i64::from(spec.footprint.universe()),
        i64::from(spec.footprint.address()),
        i64::from(spec.footprint.channels()),
        spec.manufacturer,
        spec.model,
        spec.mode_name,
        spec.fixture_path,
        Some(&label),
        spec.pinned,
    )
    .await?;
    venue_graph_db::insert_node_with_id(
        access,
        &fixture.id,
        luma_scene::venue::NodeKind::Fixture.as_str(),
        Some(&fixture.id),
        Some(&label),
    )
    .await?;
    Ok(fixture)
}

/// Delete a fixture: the paperwork *and* the thing, in the caller's transaction.
///
/// The dual of [`create`], and the only door out. Both callers — the patch
/// page's `remove_patched_fixture` and `delete_subtree` on a fixture node —
/// come through here, which is what makes "the row is gone but the resolver
/// still poses it" unrepresentable rather than merely unlikely. The node's
/// edge, its params and any constraint naming it go with it by the cascades
/// `migrations/20260829000000_venue_graph.sql` declares.
///
/// The node is dropped through [`venue_graph_db::delete_nodes`] rather than
/// with a `DELETE` of its own, because that module owns the promise that a
/// graph write drops the derived-group cache.
///
/// # Why this is a function and not a constraint
///
/// SQLite cannot express a foreign key over part of a table, so the schema-level
/// form would be `AFTER DELETE ON fixtures` deleting the node. Rejected twice
/// over: the derived-group cache is an in-process read cache over
/// `venue_nodes`, and a trigger firing behind it would leave the group tree
/// stale with nothing able to notice; and every one of these tables is guarded
/// by a `BEFORE DELETE` write-admission trigger, so SQL that runs outside an
/// armed, accepting session — a migration, a repair script — is refused rather
/// than trusted. This function is the door, and it is the only one.
///
/// Returns the number of `fixtures` rows removed, so a caller can tell a real
/// delete from a stale id.
///
/// # Errors
/// Only the database's.
pub async fn delete(access: &mut VenueAccess<'_, Write>, id: &str) -> Result<u64, PatchError> {
    venue_graph_db::delete_nodes(access, &[id.to_string()]).await?;
    Ok(fixtures_db::delete_fixture(access, id).await?)
}

/// The numbering a venue's patch list is standing at, in creation order.
///
/// # Errors
/// Fails if the patch cannot be read.
pub async fn numbering(access: &mut VenueAccess<'_, Write>) -> Result<ModelNumbering, PatchError> {
    let rows = fixtures_db::get_patched_fixtures(access).await?;
    Ok(ModelNumbering::seeded_by_labels(
        rows.iter().filter_map(|row| row.label.as_deref()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule, whole: per model, from one, in the order they were made.
    #[test]
    fn numbers_run_per_model_in_creation_order() {
        let mut numbering = ModelNumbering::default();
        let names: Vec<String> = ["Aura", "Rogue R2 Spot", "Aura", "Aura"]
            .iter()
            .map(|model| numbering.next(model))
            .collect();
        assert_eq!(names, ["Aura 1", "Rogue R2 Spot 1", "Aura 2", "Aura 3"]);
    }

    /// A venue with four movers and one par numbers the next par 2, not 6 —
    /// the property the frontend copy of this rule got right and the one thing
    /// a shared counter must not lose.
    #[test]
    fn seeding_continues_the_venues_own_count() {
        let mut numbering =
            ModelNumbering::seeded_by_labels(["Aura 1", "Aura 2", "Aura 3", "Aura 4", "Par 64 1"]);
        assert_eq!(numbering.next("Par 64"), "Par 64 2");
        assert_eq!(numbering.next("Aura"), "Aura 5");
        assert_eq!(numbering.next("Rogue R2 Spot"), "Rogue R2 Spot 1");
    }

    /// Deleting out of the middle must not hand the same name out twice.
    #[test]
    fn a_gap_does_not_reissue_a_number() {
        let mut numbering = ModelNumbering::seeded_by_labels(["Aura 1", "Aura 3"]);
        assert_eq!(numbering.next("Aura"), "Aura 4");
    }

    /// A renamed fixture is out of the counting entirely: it claims no number,
    /// and no number is minted onto it.
    #[test]
    fn a_hand_written_name_claims_nothing() {
        let mut numbering = ModelNumbering::seeded_by_labels(["House left key", "Aura 2", "Aura"]);
        assert_eq!(numbering.next("Aura"), "Aura 3");
        assert_eq!(numbering.next("House left"), "House left 1");
    }
}
