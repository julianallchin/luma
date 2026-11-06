//! The `distribute` command's argument and answer.
//!
//! [`crate::services::distribute`] owns the command and builds these directly;
//! they are the one declaration on both sides of the host boundary.

use luma_scene::distribute::Layout;
use serde::{Deserialize, Serialize};

use crate::models::venue_graph::ResolvedDangling;

/// How the caller pinned the row's layout down.
///
/// A tagged union rather than two nullable fields, because "spacing *and* a
/// span" is not a distribution anybody can mean and should not be a pair
/// anybody can send. `even` carries nothing: it is the whole face.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum DistributeLayout {
    /// Evenly across the whole face, a half-fixture margin at each end.
    Even,
    /// A fixed centre-to-centre pitch, in metres, centred on the face.
    Spacing { metres: f64 },
    /// Evenly across the window `from..to`, in **metres off the face's
    /// middle** — the same origin `At` and `alongM` use.
    Span { from: f64, to: f64 },
    /// Packed body to body, centred `metres` from the face's **middle** —
    /// signed, so `-2.0` is two metres toward the face's negative-tangent end.
    At { metres: f64 },
}

impl From<DistributeLayout> for Layout {
    fn from(layout: DistributeLayout) -> Layout {
        match layout {
            DistributeLayout::Even => Layout::Even,
            DistributeLayout::Spacing { metres } => Layout::Spacing(metres),
            DistributeLayout::Span { from, to } => Layout::Span(from, to),
            DistributeLayout::At { metres } => Layout::At(metres),
        }
    }
}

/// The inverse, for a caller that solved a preview locally and has to say what
/// it previewed. Beside the forward conversion so the two spellings of one
/// union cannot drift apart unnoticed.
impl From<Layout> for DistributeLayout {
    fn from(layout: Layout) -> DistributeLayout {
        match layout {
            Layout::Even => DistributeLayout::Even,
            Layout::Spacing(metres) => DistributeLayout::Spacing { metres },
            Layout::Span(from, to) => DistributeLayout::Span { from, to },
            Layout::At(metres) => DistributeLayout::At { metres },
        }
    }
}

/// One fixture a distribution created.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DistributedFixture {
    /// The `fixtures` row id, which is also its venue-graph node id.
    pub id: String,
    pub label: String,
    pub universe: u16,
    pub address: u16,
    /// Metres along the host face from its middle, ascending across the row.
    ///
    /// Not the same axis as the address order: addresses run along the *run*,
    /// and a face whose tangent opposes it counts the other way. Sorting a
    /// venue's fixtures by this and reading their addresses is only monotone
    /// on a face that agrees with its run.
    pub along_m: f64,
    /// Suggested placement-derived path, not a saved selector group. Empty
    /// when the derivation found no role; group authoring is explicit.
    pub group_path: Vec<String>,
}

/// Why a distribution wrote nothing.
///
/// A tagged union rather than a nullable per reason: a distribution is refused
/// for exactly one cause, and its absence is the whole of "it worked" — there
/// is no `ok` flag beside it to disagree with.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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
    /// The row would sit on top of one already on this face. The 1-D check
    /// along the face, which is the only axis a distribution moves in; two
    /// rows on *different* faces of the same truss never meet here.
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
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DistributeOccupied {
    pub label: String,
    pub from_m: f64,
    pub to_m: f64,
}

/// What one distribution did.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DistributeReport {
    pub host_node_id: String,
    pub host_socket: String,
    /// All of them, or — when [`Self::refusal`] is set — none.
    pub fixtures: Vec<DistributedFixture>,
    /// `null` is the whole of "it worked".
    pub refusal: Option<DistributeRefusal>,
    /// Whatever the solve had to decide once the row was placed, then what
    /// the layout decided for the caller: a window clipped to the face, ends
    /// held half a body inside it.
    pub warnings: Vec<String>,
    /// Open structural sockets left in the venue.
    pub dangling: Vec<ResolvedDangling>,
}
