#[tokio::main]
async fn main() {
    let port = turbofig_mcp::port_from_env();
    let addr = format!("127.0.0.1:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Turbofig daemon: failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "Turbofig daemon listening on {}",
        listener.local_addr().expect("local addr after bind")
    );
    if let Err(e) = turbofig_mcp::serve(listener).await {
        eprintln!("Turbofig daemon: server error: {e}");
        std::process::exit(1);
    }
}
