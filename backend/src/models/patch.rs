//! Addressing across the host boundary.
//!
//! [`luma_scene::patch`] owns the rule; these are its answers in the shapes
//! `serde` can carry. A projection, not a second declaration —
//! every field is read off a [`luma_scene::patch::Assignment`],
//! [`luma_scene::patch::Cell`] or [`luma_scene::patch::Note`], and no number
//! here is computed.

use luma_scene::patch::{Assignment, Cell, Note};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// One fixture's place in the patch.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PatchAssignment {
    /// The `fixtures` row id.
    pub fixture_id: String,
    pub universe: u16,
    pub address: u16,
    /// The last channel this fixture occupies.
    pub last_address: u16,
    /// The run whose universe it took, or `None` for a fixture resting on the
    /// floor.
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
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
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
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AutoPatchReport {
    /// How many fixtures ended up somewhere other than where they were.
    pub moved: usize,
    /// How many hand-set addresses it discarded on the way.
    pub overrides_discarded: usize,
    pub notes: Vec<PatchNote>,
}

/// One DMX channel of one universe, as the footprint strip draws it.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
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

/// The transport a DMX device speaks. One driver per kind.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceKind {
    /// A node on the network, reached over UDP.
    ArtNet,
    /// A dongle on this machine's USB bus.
    Usb,
}

impl DeviceKind {
    /// The `dmx_routes.device_kind` spelling, which is also the wire one.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceKind::ArtNet => "art-net",
            DeviceKind::Usb => "usb",
        }
    }
}

impl TryFrom<String> for DeviceKind {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "art-net" => Ok(DeviceKind::ArtNet),
            "usb" => Ok(DeviceKind::Usb),
            other => Err(format!("unknown output device kind '{other}'")),
        }
    }
}

/// Something DMX can be sent to: an Art-Net node, a USB dongle.
///
/// Every transport reduces to this, so the patch page draws one list whatever
/// is plugged in. A device has ports, and a port carries one universe.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OutputDevice {
    pub kind: DeviceKind,
    /// Stable across restarts and replugs, and what a route names: a node's
    /// MAC address (its IP when it announces none), or a dongle's
    /// `vid:pid:serial`, falling back to where it is plugged in.
    pub id: String,
    pub name: String,
    /// Where it is right now — an IP, a USB bus path. Shown, and kept on a
    /// route so a node can be reached before it answers its first poll.
    pub address: String,
    pub ports: Vec<OutputPort>,
    /// Takes any port number rather than a fixed list: Art-Net broadcast,
    /// which a route binds with the universe's own number.
    pub any_port: bool,
    /// Why the device is seen but cannot be driven, such as a USB permission.
    pub problem: Option<String>,
}

/// One output of an [`OutputDevice`]. For Art-Net, `port` is the 15-bit
/// Net/SubNet/Universe the node announced — never derived from a Luma
/// universe number. For USB it is the output's index.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OutputPort {
    pub port: i64,
    pub label: String,
}

/// Where one universe goes: a row of `dmx_routes`.
///
/// A universe may have several rows — the same light on a node and a dongle —
/// and a universe with none is sent nowhere.
#[derive(Serialize, Deserialize, Clone, Debug, FromRow, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DmxRoute {
    pub universe: i64,
    #[sqlx(try_from = "String")]
    pub device_kind: DeviceKind,
    pub device_id: String,
    pub port: i64,
    /// What the device called itself when it was bound, so an unplugged one
    /// is still shown by name.
    pub device_name: Option<String>,
    /// The device's address when it was bound. For Art-Net, the `ip:port` to
    /// send to until the node answers a poll with where it is now.
    pub address: Option<String>,
}
