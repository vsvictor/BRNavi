use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use quic_jsonrpc_client::QuicJsonRpcClient;
use serde_json::Value;

fn usage() -> &'static str {
    "usage: quic-jsonrpc-client <server_addr> <ca_cert_pem> '<json_rpc_request>' [--timeout-ms <millis>] [--server-name <sni>]\n\
     env: QUIC_RPC_SERVER_NAME (default: localhost), QUIC_RPC_REQUEST_TIMEOUT_MS, QUIC_RPC_HEARTBEAT_SECS"
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && matches!(args[1].as_str(), "-h" | "--help") {
        println!("{}", usage());
        return Ok(());
    }

    if args.len() < 4 {
        bail!("{}", usage());
    }

    let server_addr: SocketAddr = args[1].parse().context("invalid server_addr")?;
    let ca_cert = PathBuf::from(&args[2]);
    let request: Value = serde_json::from_str(&args[3]).context("invalid JSON request")?;

    let mut timeout_ms = std::env::var("QUIC_RPC_REQUEST_TIMEOUT_MS")
        .ok()
        .map(|value| value.parse::<u64>().context("invalid QUIC_RPC_REQUEST_TIMEOUT_MS"))
        .transpose()?;
    // Must match a SAN of the server certificate (CN/SAN of certs/server-fullchain.pem).
    let mut server_name = std::env::var("QUIC_RPC_SERVER_NAME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "localhost".to_string());

    let mut i = 4;
    while i < args.len() {
        match args[i].as_str() {
            "--server-name" => {
                server_name = args.get(i + 1).context("--server-name requires a value")?.clone();
                i += 2;
            }
            flag if flag.starts_with("--server-name=") => {
                server_name = flag
                    .split_once('=')
                    .map(|(_, value)| value.to_string())
                    .context("--server-name requires a value")?;
                i += 1;
            }
            "--timeout-ms" => {
                let value = args
                    .get(i + 1)
                    .context("--timeout-ms requires a value")?
                    .parse::<u64>()
                    .context("invalid --timeout-ms value")?;
                timeout_ms = Some(value);
                i += 2;
            }
            flag if flag.starts_with("--timeout-ms=") => {
                let value = flag
                    .split_once('=')
                    .map(|(_, value)| value)
                    .context("--timeout-ms requires a value")?
                    .parse::<u64>()
                    .context("invalid --timeout-ms value")?;
                timeout_ms = Some(value);
                i += 1;
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    let heartbeat_secs = std::env::var("QUIC_RPC_HEARTBEAT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    let client = Arc::new(QuicJsonRpcClient::connect(server_addr, &server_name, &ca_cert).await?);

    if let Some(timeout_ms) = timeout_ms {
        if timeout_ms == 0 {
            client.set_request_timeout(None).await;
        } else {
            client
                .set_request_timeout(Some(Duration::from_millis(timeout_ms)))
                .await;
        }
    }

    let _heartbeat = if heartbeat_secs > 0 {
        Some(client.clone().spawn_heartbeat(Duration::from_secs(heartbeat_secs)))
    } else {
        None
    };

    let response = client.request(request).await?;

    match response {
        Some(value) => println!("{value}"),
        None => println!("<no response>"),
    }

    if heartbeat_secs > 0 {
        eprintln!("heartbeat loop active; press Ctrl-C to exit");
        tokio::signal::ctrl_c().await.context("failed to wait for Ctrl-C")?;
    }

    Ok(())
}
