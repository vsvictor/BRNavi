//! Service catalogue: which services exist and how to start each one.

use anyhow::{Result, bail};
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    QuicJsonRpc,
}

impl Service {
    pub fn name(&self) -> &'static str {
        match self {
            Service::QuicJsonRpc => "quic-jsonrpc-server",
        }
    }

    /// Builds the service configuration from the environment and runs it until `shutdown` fires.
    pub async fn run(self, shutdown: oneshot::Receiver<()>) -> Result<()> {
        match self {
            Service::QuicJsonRpc => {
                let options = quic_jsonrpc_server::ServerOptions::from_env()?;
                quic_jsonrpc_server::run_with_shutdown(options, shutdown).await
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServiceSelection(pub Vec<Service>);

impl ServiceSelection {
    /// `TMS_SERVICES=all|rpc` or a comma-separated list.
    pub fn from_env() -> Result<Self> {
        let raw = std::env::var("TMS_SERVICES").unwrap_or_else(|_| "all".to_string());
        let mut services = Vec::new();
        for token in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            match token.to_ascii_lowercase().as_str() {
                "all" => {
                    services = vec![Service::QuicJsonRpc];
                    break;
                }
                "rpc" | "quic" | "quic-jsonrpc-server" => services.push(Service::QuicJsonRpc),
                other => bail!("TMS_SERVICES: unknown service '{other}' (expected all|rpc)"),
            }
        }
        if services.is_empty() {
            bail!("TMS_SERVICES resolved to no services");
        }
        services.dedup();
        Ok(Self(services))
    }
}
