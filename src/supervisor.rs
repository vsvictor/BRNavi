//! Supervisor: starts the selected services, fans out shutdown, reports outcomes.

use anyhow::{Result, bail};
use tokio::{sync::oneshot, task::JoinSet};
use tracing::{error, info};

use crate::services::{Service, ServiceSelection};

struct Running {
    service: Service,
    shutdown: Option<oneshot::Sender<()>>,
}

pub async fn run(selection: ServiceSelection) -> Result<()> {
    info!(
        "brnavi v{} starting services: {:?}",
        env!("CARGO_PKG_VERSION"),
        selection.0.iter().map(Service::name).collect::<Vec<_>>()
    );

    let mut tasks: JoinSet<(Service, Result<()>)> = JoinSet::new();
    let mut running = Vec::new();

    for service in selection.0 {
        let (tx, rx) = oneshot::channel();
        running.push(Running {
            service,
            shutdown: Some(tx),
        });
        tasks.spawn(async move { (service, service.run(rx).await) });
    }

    let mut failures: Vec<&'static str> = Vec::new();
    let mut shutdown_sent = false;

    let send_shutdown = |running: &mut Vec<Running>, reason: &str| {
        info!("shutting down all services ({reason})");
        for r in running.iter_mut() {
            if let Some(tx) = r.shutdown.take() {
                info!("→ stop {}", r.service.name());
                let _ = tx.send(());
            }
        }
    };

    loop {
        tokio::select! {
            _ = wait_for_signal(), if !shutdown_sent => {
                shutdown_sent = true;
                send_shutdown(&mut running, "signal received");
            }
            joined = tasks.join_next() => {
                let Some(joined) = joined else { break };
                match joined {
                    Ok((service, Ok(()))) => info!("{} exited cleanly", service.name()),
                    Ok((service, Err(err))) => {
                        error!("{} exited with error: {err:#}", service.name());
                        failures.push(service.name());
                    }
                    Err(join_err) => {
                        error!("service task panicked: {join_err}");
                        failures.push("<panicked task>");
                    }
                }
                // One service down → take the rest down too so the process restarts as a unit.
                if !shutdown_sent && !tasks.is_empty() {
                    shutdown_sent = true;
                    send_shutdown(&mut running, "sibling service stopped");
                }
            }
        }
    }

    if failures.is_empty() {
        info!("brnavi exited cleanly");
        Ok(())
    } else {
        bail!("brnavi exited with failed services: {failures:?}")
    }
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(sig) => sig,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
