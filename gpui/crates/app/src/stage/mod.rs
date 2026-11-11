//! The stage page: where a rig is **built**.
//!
//! Every other surface in the app reads a venue. This one writes it, and it is
//! the *only* one that writes a position — the patch page owns what exists and
//! how it is addressed, and there is no `set_position` anywhere
//! (`docs/specs/venue-builder-gauntlet.md`, AF8).
//!
//! # Where the state lives
//!
//! One structure, [`Build`], hung off the [`crate::visualizer::Visualizer`]
//! that is already showing the room. That is deliberate: the builder's every
//! gesture is aimed at the picture — a ghost follows the cursor over it,
//! sockets light up on it, a measurement runs across it — and the picture's
//! camera, scene and pointer already live there. A second owner would need the
//! camera copied to project a socket bead, and two cameras that can disagree
//! is how a widget ends up somewhere the pointer cannot find it.
//!
//! Within [`Build`], everything transient is [`hand::Hand`] — one state
//! machine, documented in that module.
//!
//! # Why the chrome is elements and not pixels
//!
//! The palette, the readouts, the socket beads and the measurement
//! label are all ordinary gpui elements laid over the viewport, not draws
//! inside it. Two reasons, and they are the same reason twice: the harness
//! sees elements and never pixels, and the headless harness has no renderer at
//! all — so a builder that expressed "this truss would snap here" only in the
//! picture would have no automated evidence for any of its transitions. The
//! picture carries the *shape* (`luma_render::overlay`); the element layer
//! carries the *claim*.

mod build;
mod chooser;
mod edit;
pub(crate) mod hand;
mod inspector;
mod layer;
mod page;
mod placing;
pub(crate) mod preview;
mod row;
mod scene;

pub(crate) use build::Build;
pub(crate) use inspector::selection_controls;
pub(crate) use layer::build_layer;
pub(crate) use page::{controls, elements, StagePage, StageView};
pub(crate) use scene::install;
