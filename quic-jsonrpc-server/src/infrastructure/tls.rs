//! TLS identity and QUIC server configuration.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use quinn::{ServerConfig, crypto::rustls::QuicServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, pem::PemObject};

use super::config::{ServerOptions, TlsPaths};

/// Number of TLS 1.3 resumption states kept in memory. Each reconnecting client consumes one
/// per ticket it is handed; the default (256) is far too small for a fleet of drivers whose
/// phones reconnect on every network flap and would silently disable 0‑RTT for most of them.
pub const RESUMPTION_CACHE_SIZE: usize = 16_384;
/// Tickets handed out after each handshake. Several tickets let a client perform a few 0‑RTT
/// reconnects in a row (each resumption consumes one) before it needs a full handshake again.
pub const TLS13_TICKETS_PER_HANDSHAKE: usize = 4;

pub fn build_server_config(options: &ServerOptions) -> Result<ServerConfig> {
    let (cert_chain, key) = match options.tls_paths.as_ref() {
        Some(paths) => load_cert_and_key(paths)?,
        None => generate_self_signed_cert()?,
    };

    // Built by hand (instead of `ServerConfig::with_single_cert`) so that session resumption and
    // 0‑RTT are configured explicitly:
    // * `max_early_data_size = u32::MAX` — the only non-zero value QUIC allows; without it every
    //   0‑RTT attempt from the client is rejected and the reconnect degrades to 1‑RTT.
    // * a large stateful session cache — rustls' default `NeverProducesTickets` ticketer means
    //   resumption is *stateful*; evicted entries make the client fall back to a full handshake.
    let mut crypto = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("TLS 1.3 is required for QUIC")?
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .context("invalid TLS certificate/key")?;
    crypto.max_early_data_size = u32::MAX;
    crypto.session_storage = rustls::server::ServerSessionMemoryCache::new(RESUMPTION_CACHE_SIZE);
    crypto.send_tls13_tickets = TLS13_TICKETS_PER_HANDSHAKE;

    let quic_crypto = QuicServerConfig::try_from(crypto).context("rustls config is not QUIC-compatible")?;
    let mut server_config = ServerConfig::with_crypto(Arc::new(quic_crypto));
    server_config.max_incoming(super::quic::MAX_INCOMING_CONNECTIONS);
    // Connection migration (RFC 9000 §9): accept packets for an established connection from a
    // new client address/port after path validation, without a new handshake. This is what lets
    // a phone keep its session when its IP changes (Wi‑Fi ↔ cellular, USB tethering unplug, NAT
    // rebinding). `true` is quinn's default; set explicitly so it cannot silently regress.
    server_config.migration(true);

    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(64_u32.into());

    let idle_timeout = quinn::IdleTimeout::try_from(options.idle_timeout)
        .context("failed to convert idle timeout to QUIC transport value")?;
    transport.max_idle_timeout(Some(idle_timeout));
    transport.keep_alive_interval(Some(options.keep_alive_interval));

    server_config.transport = Arc::new(transport);
    Ok(server_config)
}

fn read_pem_certs(path: &std::path::Path) -> Result<Vec<CertificateDer<'static>>> {
    CertificateDer::pem_file_iter(path)
        .with_context(|| format!("failed to read certificate file {}", path.to_string_lossy()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to parse PEM certificates in {}", path.to_string_lossy()))
}

/// Loads the server identity: `cert_path` (leaf first, intermediates may follow in the same
/// file), then the optional `chain_path` bundle of intermediate CAs. The resulting order is
/// exactly what TLS 1.3 requires in the `Certificate` message: leaf → intermediates → (root).
fn load_cert_and_key(paths: &TlsPaths) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let mut certs = read_pem_certs(&paths.cert_path)?;
    if certs.is_empty() {
        bail!("no certificates found in QUIC_RPC_CERT_PATH");
    }
    let leaf_file_len = certs.len();

    if let Some(chain_path) = paths.chain_path.as_ref() {
        let chain = read_pem_certs(chain_path)?;
        if chain.is_empty() {
            bail!("no certificates found in QUIC_RPC_CHAIN_PATH");
        }
        // Skip intermediates that are already present in the leaf file (e.g. fullchain.pem).
        for cert in chain {
            if !certs.contains(&cert) {
                certs.push(cert);
            }
        }
    }

    tracing::info!(
        chain_len = certs.len(),
        from_cert_file = leaf_file_len,
        from_chain_file = certs.len() - leaf_file_len,
        "TLS certificate chain loaded (leaf first)"
    );

    let key = PrivateKeyDer::from_pem_file(&paths.key_path)
        .with_context(|| format!("failed to read/parse private key {}", paths.key_path.to_string_lossy()))?;

    Ok((certs, key))
}

fn generate_self_signed_cert() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der = cert.cert.der().clone();
    let key_der = cert.key_pair.serialize_der();
    Ok((vec![cert_der], PrivatePkcs8KeyDer::from(key_der).into()))
}
