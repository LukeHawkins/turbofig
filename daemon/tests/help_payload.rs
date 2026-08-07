//! Integration tests for the self-describing help payload on the MCP HTTP port.
//!
//! GET / and any non-MCP path must return the help text with HTTP 200.
//! The /mcp path must not be overridden by the fallback.

use turbofig::HELP_TEXT;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Bind an ephemeral port, spawn the server, and return the base URL.
async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("read local addr");
    tokio::spawn(async move {
        turbofig::serve(listener)
            .await
            .expect("server error in test");
    });
    format!("http://{addr}")
}

/// Build a reqwest client with a generous timeout.
fn make_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("build reqwest client")
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// GET / returns HTTP 200 and the help payload.
///
/// The body must contain the tool names and key bootstrapping facts.
#[tokio::test]
async fn test_get_root_returns_help() {
    let base_url = start_server().await;
    let client = make_client();

    let res = client
        .get(format!("{base_url}/"))
        .send()
        .await
        .expect("GET /");

    assert_eq!(
        res.status(),
        reqwest::StatusCode::OK,
        "GET / must return 200"
    );

    let body = res.text().await.expect("read body");

    assert!(
        body.contains("turbofig_execute"),
        "body must contain turbofig_execute, got:\n{body}"
    );
    assert!(
        body.contains("turbofig_status"),
        "body must contain turbofig_status, got:\n{body}"
    );
    assert!(
        body.contains("inbox") || body.contains("file-bridge"),
        "body must mention the file-bridge inbox path, got:\n{body}"
    );
    assert!(
        body.contains("fileKey"),
        "body must contain fileKey, got:\n{body}"
    );
    assert!(
        body.contains("18846"),
        "body must contain the MCP port 18846, got:\n{body}"
    );

    // The body must match the exported HELP_TEXT constant exactly.
    assert_eq!(
        body, HELP_TEXT,
        "GET / body must match the HELP_TEXT constant"
    );
}

/// A non-MCP path (GET /help) also returns the help payload via the fallback.
#[tokio::test]
async fn test_fallback_returns_help() {
    let base_url = start_server().await;
    let client = make_client();

    let res = client
        .get(format!("{base_url}/help"))
        .send()
        .await
        .expect("GET /help");

    assert_eq!(
        res.status(),
        reqwest::StatusCode::OK,
        "GET /help must return 200 via the fallback handler"
    );

    let body = res.text().await.expect("read body");

    assert!(
        body.contains("turbofig_execute"),
        "fallback body must contain turbofig_execute"
    );
    assert!(
        body.contains("18846"),
        "fallback body must contain the MCP port 18846"
    );
}

/// GET /mcp must NOT return the help text.
///
/// The /mcp path belongs to the MCP service. The fallback must not shadow it.
/// We assert the response body does not contain the help marker string "18846"
/// which is present in HELP_TEXT but is not expected in a bare MCP response.
#[tokio::test]
async fn test_mcp_path_not_overridden_by_fallback() {
    let base_url = start_server().await;
    let client = make_client();

    // A bare GET /mcp is not a valid MCP request. The MCP service will return
    // a 4xx or 405. We only check that the response is NOT the help text.
    let res = client
        .get(format!("{base_url}/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .send()
        .await
        .expect("GET /mcp");

    // Must not be a 200 with the help text.
    let status = res.status();
    let body = res.text().await.expect("read body");

    // The help text marker must not appear in a bare MCP response.
    let marker = "always-on Figma design daemon";
    assert!(
        !body.contains(marker),
        "GET /mcp must not return the help text, but got body:\n{body}"
    );

    // The MCP handler must not return 200 for a bare GET (method not allowed or similar).
    assert_ne!(
        status,
        reqwest::StatusCode::OK,
        "GET /mcp must not return 200; MCP requires POST for requests"
    );
}
