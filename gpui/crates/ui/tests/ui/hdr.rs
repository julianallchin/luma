//! Run the vendored compositor's HDR10 output code and its tests on a real
//! GPU. Cargo cannot run unit tests of a vendored dependency from this
//! workspace, so the module is compiled here as it is (see `backdrop.rs`).
#![allow(dead_code)]

#[path = "../../../../vendor/zed/crates/gpui_wgpu/src/hdr.rs"]
mod hdr;

/// The compositor's own shader sources, with the linear-surface entry point,
/// must still validate: a WGSL error here only shows at window creation.
#[test]
fn compositor_shaders_with_linear_surfaces_are_valid() {
    let storage = concat!(
        include_str!("../../../../vendor/zed/crates/gpui_wgpu/src/shaders.wgsl"),
        include_str!("../../../../vendor/zed/crates/gpui_wgpu/src/srgb_extended.wgsl"),
        include_str!("../../../../vendor/zed/crates/gpui_wgpu/src/shaders_storage.wgsl"),
    );
    let webgl = concat!(
        include_str!("../../../../vendor/zed/crates/gpui_wgpu/src/shaders.wgsl"),
        include_str!("../../../../vendor/zed/crates/gpui_wgpu/src/srgb_extended.wgsl"),
        include_str!("../../../../vendor/zed/crates/gpui_wgpu/src/shaders_webgl.wgsl"),
    );
    for (name, source) in [("storage", storage), ("webgl", webgl)] {
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|error| panic!("{name}: {}", error.emit_to_string(source)));
        assert!(
            module
                .entry_points
                .iter()
                .any(|entry| entry.name == "fs_surface_linear"),
            "{name}: no fs_surface_linear"
        );
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{name}: {error:?}"));
    }
}
