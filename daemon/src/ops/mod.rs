//! The four tool ops, shared by the MCP handler and the filesystem bridge.
//!
//! Each op resolves a target connection (`routing`), does one round trip
//! through the plugin (`plugin_call`), and shapes the result. `budget` holds
//! the context-firewall size warnings both reads and screenshots share.

mod budget;
mod execute;
mod screenshot;
mod selection;
mod status;

pub use execute::run_execute;
pub use screenshot::run_screenshot;
pub use selection::run_get_selection;
pub use status::run_status;
