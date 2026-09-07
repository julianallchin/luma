//! The artifact store and the physical encodings it understands.

pub mod codecs;
pub mod store;

pub use store::{
    ArtifactDescriptor, ArtifactEncoding, ArtifactKind, ArtifactOwnership, ArtifactStore,
    ImportRequest, PlacementMethod, INPUTS_DIR, OUTPUTS_DIR, SCRATCH_DIR,
};
