//! The `distribute` command across the host boundary.
//!
//! [`crate::services::distribute`] owns the command; these are its argument and
//! its answer in the shapes `serde` and `ts-rs` can carry. A projection, not a
//! second declaration — nothing here computes.

use luma_scene::distribute::Layout;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::models::venue_graph::{warning_line, ResolvedDangling, ResolvedUnplaced};
use crate::services::distribute::{Occupied, Placed, Refusal, Report};

/// How the caller pinned the row's layout down.
///
/// A tagged union rather than two nullable fields, because "spacing *and* a
/// span" is not a distribution anybody can mean and should not be a pair
/// anybody can send. `even` carries nothing: it is the whole face.
#[derive(TS, Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[ts(export, export_to = "../../src/bindings/distribute.ts")]
#[ts(rename_all = "camelCase")]
pub enum DistributeLayout {
    /// Evenly across the whole face, a half-fixture margin at each end.
    Even,
    /// A fixed centre-to-centre pitch, in metres, centred on the face.
    Spacing { metres: f64 },
    /// Evenly across the fraction `from..to` of the face, `0` at its
    /// negative-tangent end.
    Span { from: f64, to: f64 },
}

impl From<DistributeLayout> for Layout {
    fn from(layout: DistributeLayout) -> Layout {
        match layout {
            DistributeLayout::Even => Layout::Even,
            DistributeLayout::Spacing { metres } => Layout::Spacing(metres),
            DistributeLayout::Span { from, to } => Layout::Span(from, to),
        }
    }
}

/// One fixture a distribution created.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/distribute.ts")]
#[ts(rename_all = "camelCase")]
pub struct DistributedFixture {
    /// The `fixtures` row id, which is also its venue-graph node id.
    pub id: String,
    pub label: String,
    pub universe: u16,
    pub address: u16,
    /// Metres along the host face from its middle, ascending across the row.
    pub along_m: f64,
    /// The deepest derived group it landed in, as a display path.
    pub group_path: Vec<String>,
}

impl From<&Placed> for DistributedFixture {
    fn from(placed: &Placed) -> Self {
        DistributedFixture {
            id: placed.id.clone(),
            label: placed.label.clone(),
            universe: placed.universe,
            address: placed.address,
            along_m: placed.along_m,
            group_path: placed.group_path.clone(),
        }
    }
}

/// Why a distribution wrote nothing.
///
/// A tagged union rather than a nullable per reason: a distribution is refused
/// for exactly one cause, and its absence is the whole of "it worked" — there
/// is no `ok` flag beside it to disagree with.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/bindings/distribute.ts")]
#[ts(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DistributeRefusal {
    /// The row is longer than the face.
    TooLong {
        /// How long the host face would have to be. Already a length the host
        /// can be built at, so it can be passed straight to `set_params`.
        needed_m: f64,
        /// How long it is now.
        available_m: f64,
        /// The node to change the length of.
        extend_node_id: String,
        /// The fix, in words, for a page that shows the refusal rather than
        /// acting on it.
        suggestion: String,
    },
    /// The row would sit on top of one already on this face.
    Overlap {
        /// The metres along the face the row would have claimed, from its
        /// middle.
        from_m: f64,
        to_m: f64,
        /// The fixtures in the way, in face order.
        held_by: Vec<DistributeOccupied>,
        suggestion: String,
    },
}

/// One stretch of a host face that is already spoken for.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/distribute.ts")]
#[ts(rename_all = "camelCase")]
pub struct DistributeOccupied {
    pub label: String,
    pub from_m: f64,
    pub to_m: f64,
}

impl From<&Occupied> for DistributeOccupied {
    fn from(held: &Occupied) -> Self {
        DistributeOccupied {
            label: held.label.clone(),
            from_m: held.from_m,
            to_m: held.to_m,
        }
    }
}

impl From<&Refusal> for DistributeRefusal {
    fn from(refusal: &Refusal) -> Self {
        match refusal {
            Refusal::TooLong(fit) => DistributeRefusal::TooLong {
                needed_m: fit.needed_m,
                available_m: fit.available_m,
                extend_node_id: fit.extend_node.clone(),
                suggestion: format!(
                    "needs {:.2} m, the face is {:.2} m — extend({}, to={:.2})",
                    fit.needed_m, fit.available_m, fit.extend_node, fit.needed_m
                ),
            },
            Refusal::Overlap {
                from_m,
                to_m,
                held_by,
            } => DistributeRefusal::Overlap {
                from_m: *from_m,
                to_m: *to_m,
                held_by: held_by.iter().map(DistributeOccupied::from).collect(),
                suggestion: format!(
                    "{from_m:.2}..{to_m:.2} m of this face is already held by {} — \
                     distribute into a span that clears it",
                    held_by
                        .iter()
                        .map(|held| held.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
        }
    }
}

/// What one distribution did.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/distribute.ts")]
#[ts(rename_all = "camelCase")]
pub struct DistributeReport {
    pub host_node_id: String,
    pub host_socket: String,
    /// All of them, or — when [`Self::refusal`] is set — none.
    pub fixtures: Vec<DistributedFixture>,
    /// `null` is the whole of "it worked".
    pub refusal: Option<DistributeRefusal>,
    pub warnings: Vec<String>,
    /// Open structural sockets left in the venue.
    pub dangling: Vec<ResolvedDangling>,
    /// Subtrees the solve could not reach — the tray, and anything detached.
    pub unplaced: Vec<ResolvedUnplaced>,
}

impl From<Report> for DistributeReport {
    fn from(report: Report) -> Self {
        DistributeReport {
            host_node_id: report.host_node,
            host_socket: report.host_socket,
            fixtures: report
                .fixtures
                .iter()
                .map(DistributedFixture::from)
                .collect(),
            refusal: report.refusal.as_ref().map(DistributeRefusal::from),
            warnings: report.warnings.iter().map(warning_line).collect(),
            dangling: report.dangling.iter().map(ResolvedDangling::from).collect(),
            unplaced: report.unplaced.iter().map(ResolvedUnplaced::from).collect(),
        }
    }
}
