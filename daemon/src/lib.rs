use rmcp::{
    model::*,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData as McpError, ServerHandler,
};
use serde_json::json;

/// Parse a port number from an optional string value.
/// Returns 3846 when the input is None, cannot be parsed as u16, or is zero.
fn port_from_str(s: Option<&str>) -> u16 {
    s.and_then(|v| v.parse::<u16>().ok())
        .filter(|&p| p != 0)
        .unwrap_or(3846)
}

/// Read the MCP port from TURBOFIG_MCP_PORT. Default is 3846.
pub fn port_from_env() -> u16 {
    port_from_str(std::env::var("TURBOFIG_MCP_PORT").ok().as_deref())
}

/// MCP handler that exposes the turbofig_status tool.
///
/// The #[tool_router] macro generates a static tool_router() constructor.
/// No instance field is needed: #[tool_handler] calls Self::tool_router()
/// on each dispatch.
#[derive(Clone)]
pub struct StatusHandler;

impl Default for StatusHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl StatusHandler {
    pub fn new() -> Self {
        Self
    }

    /// Return daemon liveness status as JSON.
    #[tool(description = "Return daemon status")]
    fn turbofig_status(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(
            json!({"ok": true}).to_string(),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for StatusHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
    }
}

/// Build the axum router with the MCP service mounted at /mcp.
pub fn build_router() -> axum::Router {
    // StreamableHttpServerConfig is #[non_exhaustive], so construct via Default
    // then set the field directly.
    let mut config = StreamableHttpServerConfig::default();
    // Require mcp-session-id on all non-initialize requests.
    config.legacy_session_mode = true;
    let service = StreamableHttpService::new(
        || Ok(StatusHandler::new()),
        LocalSessionManager::default().into(),
        config,
    );
    axum::Router::new().nest_service("/mcp", service)
}

/// Serve the MCP router on the given TCP listener.
pub async fn serve(listener: tokio::net::TcpListener) -> std::io::Result<()> {
    axum::serve(listener, build_router()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_defaults_to_3846_when_unset() {
        assert_eq!(port_from_str(None), 3846);
    }

    #[test]
    fn port_parses_valid_number() {
        assert_eq!(port_from_str(Some("9000")), 9000);
    }

    #[test]
    fn port_falls_back_on_garbage_input() {
        assert_eq!(port_from_str(Some("notaport")), 3846);
    }

    #[test]
    fn port_rejects_zero_and_falls_back() {
        // Port 0 means an OS-assigned ephemeral port, never a meaningful daemon port.
        // Reject it and fall back to 3846.
        assert_eq!(port_from_str(Some("0")), 3846);
    }
}
