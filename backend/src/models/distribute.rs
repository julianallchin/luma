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
    /// `across` is the other way over a surface, in metres off its middle:
    /// with `metres` it names a point on a floor or a deck.
    At {
        metres: f64,
        #[serde(default)]
        across: f64,
    },
    /// One light at a point in plan: `(u, v)` stage metres from the middle of
    /// a level face, in the **room's** directions whichever way the host is
    /// turned, lifted `trim` metres above it. This is how a person pointing
    /// at a floor and an agent naming a coordinate both say "here"; the stage
    /// turns it into the face's own along and across before the row is laid.
    Point {
        u: f64,
        v: f64,
        #[serde(default)]
        trim: f64,
    },
}

/// A point in plan that has yet to be put on a face. See
/// [`DistributeLayout::Point`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanPoint {
    pub u: f64,
    pub v: f64,
    pub trim: f64,
}

impl DistributeLayout {
    /// The row along its face and how far across it sits, in metres.
    ///
    /// # Errors
    /// The point itself, for a [`DistributeLayout::Point`]: only a caller
    /// holding the venue can say where on the face that is.
    pub fn on_face(self) -> Result<(Layout, f64), PlanPoint> {
        match self {
            DistributeLayout::Even => Ok((Layout::Even, 0.0)),
            DistributeLayout::Spacing { metres } => Ok((Layout::Spacing(metres), 0.0)),
            DistributeLayout::Span { from, to } => Ok((Layout::Span(from, to), 0.0)),
            DistributeLayout::At { metres, across } => Ok((Layout::At(metres), across)),
            DistributeLayout::Point { u, v, trim } => Err(PlanPoint { u, v, trim }),
        }
    }
}

/// The inverse of [`DistributeLayout::on_face`], for a caller that solved a
/// preview locally and has to say what it previewed. Beside the forward
/// conversion so the two spellings of one union cannot drift apart unnoticed.
impl From<Layout> for DistributeLayout {
    fn from(layout: Layout) -> DistributeLayout {
        match layout {
            Layout::Even => DistributeLayout::Even,
            Layout::Spacing(metres) => DistributeLayout::Spacing { metres },
            Layout::Span(from, to) => DistributeLayout::Span { from, to },
            Layout::At(metres) => DistributeLayout::At {
                metres,
                across: 0.0,
            },
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
    /// Where it is patched; `None` for a fixture with no DMX channels (a
    /// laser projector).
    pub universe: Option<u16>,
    pub address: Option<u16>,
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
