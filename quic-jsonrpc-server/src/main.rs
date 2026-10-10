use anyhow::Result;
use quic_jsonrpc_server::{ServerOptions, env, run_with_shutdown};

fn main() -> Result<()> {
    // Environment mutation happens here, strictly single-threaded, before tokio spawns workers.
    // Only this service's own `.env` (quic-jsonrpc-server/.env) is consulted.
    let dotenv_paths = env::load_dotenv();

    // Both `ring` (via reqwest) and `aws-lc-rs` (via quinn) are in the dependency graph, so rustls
    // cannot pick a process-wide provider automatically. Select one explicitly before any TLS setup.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(env::log_filter())
        .with_file(true)
        .with_line_number(true)
        .with_target(true)
        .with_thread_ids(true)
        .compact()
        .init();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(dotenv_paths))
}

async fn run(dotenv_paths: Vec<std::path::PathBuf>) -> Result<()> {
    if dotenv_paths.is_empty() {
        tracing::warn!(
            ".env not found in {} or next to the executable — using built-in defaults and process env only",
            env::crate_dir().display()
        );
    }
    for path in &dotenv_paths {
        tracing::info!("loaded .env from {}", path.display());
    }

    let options = ServerOptions::from_env()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        if wait_for_shutdown_signal().await {
            let _ = shutdown_tx.send(());
        }
    });

    run_with_shutdown(options, shutdown_rx).await
}

async fn wait_for_shutdown_signal() -> bool {
    #[cfg(unix)]
    {
        let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(sig) => sig,
            Err(_) => return false,
        };

        tokio::select! {
            result = tokio::signal::ctrl_c() => result.is_ok(),
            _ = sigterm.recv() => true,
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.is_ok()
    }
}
