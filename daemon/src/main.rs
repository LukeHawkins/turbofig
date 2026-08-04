#[tokio::main]
async fn main() {
    let port = turbofig_mcp::port_from_env();
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
    println!(
        "Turbofig daemon listening on {}",
        listener.local_addr().unwrap()
    );
    turbofig_mcp::serve(listener).await.expect("server error");
}
