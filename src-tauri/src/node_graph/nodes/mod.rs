use crate::models::node_graph::*;

mod analysis;
mod apply;
mod audio;
mod color;
mod selection;
mod signals;
mod spatial;

pub fn get_node_types() -> Vec<NodeTypeDef> {
    let mut types = Vec::new();
    types.extend(selection::get_node_types());
    types.extend(audio::get_node_types());
    types.extend(signals::get_node_types());
    types.extend(color::get_node_types());
    types.extend(spatial::get_node_types());
    types.extend(apply::get_node_types());
    types.extend(analysis::get_node_types());
    types
}
