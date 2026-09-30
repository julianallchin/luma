//! The `luma-render` integration tests, as one binary.
//!
//! `cargo test -p luma-render --test render <filter>`; a filter such as
//! `venue_poses::` runs one file.
//!
//! Four tests stay in their own binaries next to this directory, because each
//! changes process-wide state that every other test here would then see:
//! `hdr_dither` and `hdr_presentation` adopt a compositor device,
//! `waveform` adopts a device with default limits, and `shared_presentation`
//! sets an environment variable that selects the presentation fallback.

mod common;

mod aerial_perspective;
mod cable_horizon;
mod camera_export;
mod environment_controls;
mod fixture_kinematics_contract;
mod floor_lighting;
mod floor_materials;
mod footage;
mod gizmo_pivot;
mod golden_descriptors;
mod horizon_seam;
mod lens_footprint;
mod outdoor_haze;
mod post_look;
mod procedural_haze;
mod renderer_contract_goldens;
mod resize_probe;
mod sky_clouds;
mod stall_probe;
mod sun_shadow_contact;
mod sun_shadow_export_views;
mod sun_shadow_orbit;
mod sun_shadow_stage_orbit;
mod timestamp_lie;
mod timestamp_query_contract;
mod venue_build;
mod venue_poses;
mod volumetric_transport;
