#![cfg(all(feature = "app", feature = "pixel"))]

#[test]
fn graph_gestures_with_real_text_and_rendering() {
    super::support::graph_interactions::exercise(
        gpui_agent::Mode::Pixel,
        "graph-wire-search-pixels",
    );
}
