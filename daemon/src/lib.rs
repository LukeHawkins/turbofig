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
//! `config` reads every env-driven setting. `token` generates and persists
//! the WS pairing token; `embedded` embeds the built Figma plugin into the
//! binary at compile time; `plugin_files` writes that embedded plugin out to
//! `~/.turbofig/figma-plugin/` with the real token injected. `control` is the
//! authenticated local `/control` path a caller uses to drain and restart
//! the daemon. `spawn` starts a detached `turbofig serve` in its own
//! session; `proxy` is `turbofig mcp`, a stdio MCP server that forwards
//! every tool call onto `POST /job`, starting the daemon via `spawn` when
//! it is not already running. `first_run` holds the text and the clipboard/
//! Figma-launch seams for the bare `turbofig` command (no subcommand).

pub mod cli;
pub mod first_run;
pub mod launchd;
pub mod proxy;
pub mod spawn;
pub mod supervisor;

mod bridge;
mod config;
mod control;
mod embedded;
mod image;
mod mcp;
mod ops;
mod plugin_call;
mod plugin_files;
mod routing;
mod state;
mod token;
mod ws;

pub use bridge::serve_bridge;
pub use config::{
    bridge_dir_display, bridge_dir_from_env, port_from_env, request_timeout_from_env,
    ws_port_from_env,
};
pub use embedded::{
    embedded_plugin, ui_html_contains_a_real_token, ui_html_has_placeholder, EmbeddedPlugin,
};
pub use mcp::{build_router, serve, serve_with_state, HELP_TEXT};
pub use ops::{run_execute, run_get_selection, run_screenshot, run_status};
pub use plugin_files::{plugin_files_outdated, write_plugin_files};
pub use state::AppState;
pub use token::ensure_token;
pub use ws::serve_ws;
