use std::sync::Arc;
use turbofig::AppState;

#[tokio::main]
async fn main() {
    let mcp_port = turbofig::port_from_env();
    let ws_port = turbofig::ws_port_from_env();
    let bridge_dir = turbofig::bridge_dir_from_env();

    let mcp_addr = format!("127.0.0.1:{mcp_port}");
    let ws_addr = format!("127.0.0.1:{ws_port}");

    let mcp_listener = match tokio::net::TcpListener::bind(&mcp_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Turbofig daemon: failed to bind MCP port {mcp_addr}: {e}");
            std::process::exit(1);
        }
    };

    let ws_listener = match tokio::net::TcpListener::bind(&ws_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Turbofig daemon: failed to bind WS port {ws_addr}: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "Turbofig MCP listening on {}",
        mcp_listener.local_addr().expect("local addr after bind")
    );
    println!(
        "Turbofig WS  listening on {}",
        ws_listener.local_addr().expect("local addr after bind")
    );
    println!("Turbofig bridge dir: {}", bridge_dir.display());

    let state = Arc::new(AppState::new());

    let mcp_state = state.clone();
    let mcp_handle = tokio::spawn(async move {
        if let Err(e) = turbofig::serve_with_state(mcp_listener, mcp_state).await {
            eprintln!("Turbofig daemon: MCP server error: {e}");
            std::process::exit(1);
        }
    });

    let ws_state = state.clone();
    let ws_handle = tokio::spawn(async move {
        if let Err(e) = turbofig::serve_ws(ws_listener, ws_state).await {
            eprintln!("Turbofig daemon: WS server error: {e}");
            std::process::exit(1);
        }
    });

    let bridge_state = state.clone();
    let bridge_handle = tokio::spawn(async move {
        if let Err(e) = turbofig::serve_bridge(bridge_state, bridge_dir).await {
            eprintln!("Turbofig daemon: bridge error: {e}");
            std::process::exit(1);
        }
    });

    // A healthy daemon runs forever. Any handle that completes — whether by a
    // normal return (unexpected for a server), by an Err path that did not call
    // process::exit, or by a task panic surfacing as a JoinError — means a
    // subsystem is dead. Log the name and exit so launchd KeepAlive restarts.
    tokio::select! {
        res = mcp_handle => {
            match res {
                Ok(()) => eprintln!("Turbofig daemon: MCP server task ended unexpectedly"),
                Err(e) => eprintln!("Turbofig daemon: MCP server task panicked: {e}"),
            }
        }
        res = ws_handle => {
            match res {
                Ok(()) => eprintln!("Turbofig daemon: WS server task ended unexpectedly"),
                Err(e) => eprintln!("Turbofig daemon: WS server task panicked: {e}"),
            }
        }
        res = bridge_handle => {
            match res {
                Ok(()) => eprintln!("Turbofig daemon: bridge task ended unexpectedly"),
                Err(e) => eprintln!("Turbofig daemon: bridge task panicked: {e}"),
            }
        }
    }
    std::process::exit(1);
}
