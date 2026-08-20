use crate::dispatch::{AppServices, CommandError};
use crate::models::node_graph::NodeTypeDef;

/// The node-type catalogue is a pure in-memory read; it takes no services at
/// all. Handlers stay uniform in shape anyway so the registry can generate both
/// adapters without special cases.
pub async fn get_node_types(_services: &AppServices) -> Result<Vec<NodeTypeDef>, CommandError> {
    Ok(crate::node_graph::nodes::get_node_types())
}
