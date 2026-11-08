//! stdout is reserved for MCP. The bridge never constructs an application core.
#[cfg(unix)]
#[tokio::main]
async fn main() {
    use rmcp::ServiceExt;
    let mut args = std::env::args_os().skip(1);
    let mut connection = None;
    let mut name = "External MCP client".to_string();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--connection") => connection = args.next().map(std::path::PathBuf::from),
            Some("--client-name") => {
                name = args
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .unwrap_or_default()
            }
            Some("--help") => {
                eprintln!(
                    "cadpad-mcp --connection <Cadpad connection.json> [--client-name <label>]\nStart the Cadpad desktop app first. Copy the connection configuration from Settings."
                );
                return;
            }
            _ => {
                eprintln!("Unknown argument. Use cadpad-mcp --help.");
                std::process::exit(2);
            }
        }
    }
    let Some(connection) = connection else {
        eprintln!(
            "A connection file is required. Start Cadpad and copy the MCP configuration from Settings."
        );
        std::process::exit(2);
    };
    let handler = cadpad_mcp::mcp::Bridge::new(connection, name);
    match handler.serve(rmcp::transport::stdio()).await {
        Ok(service) => {
            let _ = service.waiting().await;
        }
        Err(error) => {
            eprintln!("MCP session failed: {error}");
            std::process::exit(1);
        }
    }
}
#[cfg(not(unix))]
fn main() {
    eprintln!(
        "This build supports local MCP on macOS/Linux. Windows named-pipe transport is not available yet."
    );
    std::process::exit(1);
}
