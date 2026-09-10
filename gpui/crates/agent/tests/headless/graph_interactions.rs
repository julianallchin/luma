#[test]
fn wire_search_and_delete_through_native_graph_gestures() {
    super::support::graph_interactions::exercise(gpui_agent::Mode::Headless, "graph-wire-search");
}
