//! The harness's own tests: the pump, the interpreter and the MCP protocol,
//! exercised without the Luma app.
//!
//! These need no library and no renderer, so they are the one group that runs
//! in full with no features turned on.

mod harness;
mod mcp;
