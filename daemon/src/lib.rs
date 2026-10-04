//! turbofig daemon: one always-on Rust process bridging an AI client (via the
//! filesystem bridge, MCP HTTP, or native MCP) to a running Figma plugin over
//! WebSocket.
//!
//! Three servers share one [`AppState`]: the MCP HTTP endpoint (`mcp`), the
//! plugin WebSocket server (`ws`), and the filesystem bridge (`bridge`).
//! `routing` resolves which connected plugin a call targets; `plugin_call` is
//! the one send/await/timeout path the four tool ops share; `ops` holds those
//! four ops (`run_status`, `run_execute`, `run_get_selection`,
//! `run_screenshot`); `image` handles screenshot PNG dimensions and resizing;
//! `config` reads every env-driven setting.

mod bridge;
mod config;
mod image;
mod mcp;
mod ops;
mod plugin_call;
mod routing;
mod state;
mod ws;

pub use bridge::serve_bridge;
pub use config::{bridge_dir_from_env, port_from_env, request_timeout_from_env, ws_port_from_env};
pub use mcp::{build_router, serve, serve_with_state, HELP_TEXT};
pub use ops::{run_execute, run_get_selection, run_screenshot, run_status};
pub use state::AppState;
pub use ws::serve_ws;
