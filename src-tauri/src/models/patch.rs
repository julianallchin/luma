//! Addressing across the host boundary.
//!
//! [`luma_scene::patch`] owns the rule; these are its answers in the shapes
//! `ts-rs` and `serde` can carry. A projection, not a second declaration —
//! every field is read off a [`luma_scene::patch::Assignment`],
//! [`luma_scene::patch::Cell`] or [`luma_scene::patch::Note`], and no number
//! here is computed.

use luma_scene::patch::{Assignment, Cell, Note};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

/// One fixture's place in the patch.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/patch.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatchAssignment {
    /// The `fixtures` row id.
    pub fixture_id: String,
    pub universe: u16,
    pub address: u16,
    /// The last channel this fixture occupies.
    pub last_address: u16,
    /// The run whose universe it took, or `None` for a fixture in the tray or
    /// resting on the floor.
    pub run: Option<String>,
    /// A hand-set address the allocator preserved rather than derived.
    pub pinned: bool,
}

impl From<&Assignment> for PatchAssignment {
    fn from(assignment: &Assignment) -> Self {
        PatchAssignment {
            fixture_id: assignment.fixture.clone(),
            universe: assignment.footprint.universe(),
            address: assignment.footprint.address(),
            last_address: assignment.footprint.last(),
            run: assignment.run.clone(),
            pinned: assignment.pinned,
        }
    }
}

/// Something an allocation had to decide, as prose for the patch page.
///
/// Flattened to a sentence rather than carried as a tagged union: the page
/// shows these in a list, nothing branches on them, and a variant per note
/// would be a second copy of an enum that already exists in `luma_scene`.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/patch.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatchNote {
    pub message: String,
}

impl From<&Note> for PatchNote {
    fn from(note: &Note) -> Self {
        PatchNote {
            message: match note {
                Note::RunRolled {
                    run,
                    offered,
                    taken,
                } => format!(
                    "{run} does not fit in universe {offered}; the whole run moved to universe {taken}"
                ),
                Note::RunSplit { run, universes } => format!(
                    "{run} needs more than one universe: {}",
                    universes
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Note::NoRoom { fixture } => {
                    format!("no free address anywhere for {fixture}")
                }
            },
        }
    }
}

/// What one auto-patch did.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/patch.ts")]
#[ts(rename_all = "camelCase")]
pub struct AutoPatchReport {
    /// How many fixtures ended up somewhere other than where they were.
    pub moved: usize,
    /// How many hand-set addresses it discarded on the way.
    pub overrides_discarded: usize,
    pub notes: Vec<PatchNote>,
}

/// One DMX channel of one universe, as the footprint strip draws it.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/patch.ts")]
#[ts(rename_all = "camelCase")]
pub struct UniverseCell {
    /// `1..=512`.
    pub address: u16,
    pub fixture_id: Option<String>,
    pub label: Option<String>,
    /// Which channel of that fixture this is, zero-based.
    pub channel: u16,
    /// More than one fixture claims it.
    pub collision: bool,
    pub pinned: bool,
}

impl UniverseCell {
    /// The cell at `address` (1-based), given what the occupancy says holds it.
    pub(crate) fn new(
        address: u16,
        cell: &Cell,
        label: impl FnOnce(&str) -> (Option<String>, bool),
    ) -> UniverseCell {
        let (label, pinned) = cell
            .fixture
            .as_deref()
            .map_or((None, false), |id| label(id));
        UniverseCell {
            address,
            fixture_id: cell.fixture.clone(),
            label,
            channel: cell.channel,
            collision: cell.collision,
            pinned,
        }
    }
}

/// An Art-Net node that answered a poll.
///
/// A wire model beside [`UniverseOutput`] rather than a struct inside the
/// sender, because it is half of a binding: the outputs table names a node, and
/// the two halves of one decision should be described in one place. `port_address`
/// is the node's **own** announced Net/SubNet/Universe — never derived from a
/// Luma universe number.
#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/patch.ts")]
#[ts(rename_all = "camelCase")]
pub struct ArtNetNode {
    pub ip: String,
    pub name: String,
    pub long_name: String,
    pub port_address: u16,
    /// Unix seconds at the last reply.
    pub last_seen: u64,
}

/// Where one universe goes on the wire: a row of `universe_outputs`.
///
/// The table that replaces `(net << 8) | (subnet << 4) | (universe & 0xF)`.
/// That arithmetic aliases universe 17 onto universe 1 and cannot name a second
/// node at all; a binding names one.
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/patch.ts")]
#[ts(rename_all = "camelCase")]
pub struct UniverseOutput {
    pub universe: i64,
    pub node_ip: String,
    pub node_port: i64,
    /// Art-Net's 15-bit Net/SubNet/Universe triple, as the node announced it.
    pub port_address: i64,
    pub node_name: Option<String>,
}

/// A free slot, as [`crate::services::patch::next_addresses`] hands it to a
/// caller whose fixtures do not exist yet.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/patch.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatchAddress {
    pub universe: u16,
    pub address: u16,
    pub last_address: u16,
}

impl From<luma_scene::patch::Footprint> for PatchAddress {
    fn from(footprint: luma_scene::patch::Footprint) -> Self {
        PatchAddress {
            universe: footprint.universe(),
            address: footprint.address(),
            last_address: footprint.last(),
        }
    }
}
