//! Dump the node-type catalogue as JSON — the same `get_node_types()` the app
//! serves to the editor over IPC. The graph-editor capture harness needs the
//! catalogue offline (ports and param defs are compiled in, not stored in a
//! saved graph), so it reads `harness/gauntlet/node-types.json` produced here.

fn main() {
    let types = luma_lib::node_graph::nodes::get_node_types();
    println!(
        "{}",
        serde_json::to_string_pretty(&types).expect("node types serialize")
    );
}
