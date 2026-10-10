//! `brnavi` — orchestrator for the BRNavi backend.
//!
//! Runs the services inside one Tokio runtime and supervises them:
//!
//! * `quic-jsonrpc-server` — JSON-RPC 2.0 over QUIC (driver login / shifts).
//!
//! Responsibilities: let each service load its own `.env`, initialise logging once, build each
//! service's configuration, start them, propagate SIGINT/SIGTERM as a coordinated shutdown and
//! exit non-zero if a service fails. Which services run is controlled by
//! `TMS_SERVICES` (`all` | `rpc`, default `all`).

mod env;
mod services;
mod supervisor;

use anyhow::Result;

use services::ServiceSelection;

fn main() -> Result<()> {
    // Environment mutation must happen single-threaded, before the runtime spawns workers.
    // Each service crate loads (and anchors paths in) its own `<crate>/.env`.
    let loaded = env::load_dotenv_files();

    // Both `ring` (via reqwest) and `aws-lc-rs` are in the dependency graph, so rustls cannot pick
    // a process-wide provider automatically.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // `TMS_RUST_LOG` wins. Otherwise compose a filter from what each service's own `.env`
    // declared (`QUIC_RPC_RUST_LOG`) plus the orchestrator itself.
    let filter = env::log_filter();

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(true)
        .compact()
        .init();

    for path in &loaded {
        tracing::info!("loaded .env from {}", path.display());
    }

    let selection = ServiceSelection::from_env()?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(supervisor::run(selection))
}
