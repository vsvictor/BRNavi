use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use once_cell::sync::Lazy;
use quinn::ClientConfig;
use rustls::pki_types::{CertificateDer, pem::PemObject};

/// How often the client sends QUIC PING frames to keep NAT bindings and the connection alive.
pub const DEFAULT_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// How long the connection may go without receiving a single packet before QUIC declares the
/// peer dead.
///
/// This is a dead-peer detector, not a session lifetime: both sides send keep-alive PINGs every
/// [`DEFAULT_KEEP_ALIVE_INTERVAL`], so a healthy connection never approaches it even with hours
/// of application silence. QUIC negotiates the *minimum* of both peers' values (RFC 9000 §10.1);
/// this matches the server's `QUIC_RPC_IDLE_TIMEOUT_SECS` (60 s) so that
/// * the server reaps a vanished phone after a minute instead of holding a half-open connection
///   for the session TTL, and
/// * the client notices a black-holed path by itself within a minute even when the app-level
///   heartbeat is off; the next `request` then reconnects (0‑RTT + `rpc.resume`).
///
/// Android Doze: while the radio is parked the connection will time out on both ends. That is
/// intended — the session survives server-side and is re-attached transparently on wake.
pub const DEFAULT_MAX_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound for a single QUIC handshake (initial connect or reconnect). Without it a
/// connect attempt towards an unreachable host would only fail after the idle timeout.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound for the 0‑RTT confirmation (`ZeroRttAccepted`) after an early-data reconnect.
/// Normally this is one RTT; the bound only protects against a server that never finishes the
/// handshake.
pub const DEFAULT_ZERO_RTT_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-process cache of client configs keyed by CA path.
///
/// The `quinn::ClientConfig` wraps an `Arc<rustls::ClientConfig>` whose in-memory
/// `Resumption` store holds the TLS session tickets issued by the server. Rebuilding the config on
/// every `connect` (what happened before) throws those tickets away, so a reconnect could never
/// use 0‑RTT. Sharing the config keeps the tickets for the lifetime of the process — including
/// across `quic_rpc_client_close` → `quic_rpc_client_connect` cycles driven from Dart.
static CLIENT_CONFIGS: Lazy<Mutex<HashMap<PathBuf, ClientConfig>>> = Lazy::new(|| Mutex::new(HashMap::new()));

pub fn build_client_config(ca_cert_path: &Path) -> Result<ClientConfig> {
    let key = ca_cert_path.to_path_buf();
    if let Ok(cache) = CLIENT_CONFIGS.lock()
        && let Some(config) = cache.get(&key)
    {
        return Ok(config.clone());
    }

    let config = build_client_config_uncached(ca_cert_path)?;
    if let Ok(mut cache) = CLIENT_CONFIGS.lock() {
        // Another thread may have raced us; keep the first one so its ticket store wins.
        return Ok(cache.entry(key).or_insert(config).clone());
    }
    Ok(config)
}

fn build_client_config_uncached(ca_cert_path: &Path) -> Result<ClientConfig> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(ca_cert_path)
        .with_context(|| format!("failed to read CA certificate {}", ca_cert_path.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to parse CA PEM file")?;

    if certs.is_empty() {
        bail!("CA PEM does not contain certificates");
    }

    let mut roots = rustls::RootCertStore::empty();
    for cert in certs {
        roots.add(cert).context("failed to add CA certificate")?;
    }

    // `with_root_certificates` builds a TLS 1.3-only rustls config with `enable_early_data = true`
    // and the default in-memory resumption store, i.e. everything 0‑RTT needs on the client.
    let mut client_config = ClientConfig::with_root_certificates(Arc::new(roots))?;

    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(DEFAULT_KEEP_ALIVE_INTERVAL));
    let idle_timeout = quinn::IdleTimeout::try_from(DEFAULT_MAX_IDLE_TIMEOUT)
        .context("failed to convert idle timeout to QUIC transport value")?;
    transport.max_idle_timeout(Some(idle_timeout));
    client_config.transport_config(Arc::new(transport));

    Ok(client_config)
}
