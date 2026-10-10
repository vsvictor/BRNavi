//! JSON-RPC 2.0 over raw QUIC — driver authentication / shift proxy for the TMS mobile client.
//!
//! Layout (Clean Architecture, dependencies point inwards):
//!
//! ```text
//! presentation (MVC: model / view / controller)  ──▶  application (use cases)  ──▶  domain
//!            ▲                                                 ▲
//!            └──────────── infrastructure (QUIC, Mongo, HTTP upstream, config, TLS) ──┘
//! ```
//!
//! [`run_with_shutdown`] is the composition root: it wires concrete adapters into the use cases
//! and hands the assembled dispatcher to the QUIC transport.

pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod presentation;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use anyhow::Result;

use application::{AuthService, Metrics, RateLimiter, ShiftService};
use domain::SessionRepository;
use infrastructure::{
    config::MongoOptions,
    http::HttpDriverApiGateway,
    persistence::{InMemorySessionRepository, MongoAuthenticator, MongoSessionRepository},
    quic::QuicServer,
};
use presentation::RpcDispatcher;

pub use application::RateLimitConfig;
pub use infrastructure::config::{ServerOptions, SessionBackend, TlsPaths, UpstreamOptions};
pub use infrastructure::env;

/// Builds the session repository selected by configuration.
async fn build_session_repository(options: &ServerOptions) -> Result<Arc<dyn SessionRepository>> {
    Ok(match options.session_backend {
        SessionBackend::Memory => Arc::new(InMemorySessionRepository::new(options.session_ttl)),
        SessionBackend::Mongo => {
            Arc::new(MongoSessionRepository::connect(&MongoOptions::from_env()?, options.session_ttl).await?)
        }
    })
}

/// Starts the server and runs until `shutdown` fires or the endpoint closes.
///
/// Upstream (driver API) parameters are read from the environment via
/// [`UpstreamOptions::from_env`] at this point; the tile proxy via [`map_tiles::from_env`].
pub async fn run_with_shutdown(options: ServerOptions, shutdown: tokio::sync::oneshot::Receiver<()>) -> Result<()> {
    let mongo = MongoOptions::from_env()?;
    let authenticator = Arc::new(MongoAuthenticator::connect(&mongo, options.session_ttl).await?);
    let vehicles = Arc::new(infrastructure::persistence::vehicles::MongoVehicleRepository::connect(&mongo).await?);
    run_composed(options, shutdown, authenticator, Some(vehicles)).await
}

/// Explicit dependency injection for transport tests/embedded servers. The normal startup
/// path above always uses MongoDB credentials, regardless of the session storage backend.
pub async fn run_with_authenticator(
    options: ServerOptions,
    shutdown: tokio::sync::oneshot::Receiver<()>,
    authenticator: Arc<dyn domain::Authenticator>,
) -> Result<()> {
    run_composed(options, shutdown, authenticator, None).await
}

async fn run_composed(
    options: ServerOptions,
    shutdown: tokio::sync::oneshot::Receiver<()>,
    authenticator: Arc<dyn domain::Authenticator>,
    vehicles: Option<Arc<dyn domain::vehicle::VehicleRepository>>,
) -> Result<()> {
    let sessions = build_session_repository(&options).await?;
    let vehicle_service = vehicles.map(|repository| {
        Arc::new(application::vehicle::VehicleService::new(
            Arc::clone(&sessions),
            repository,
        ))
    });
    let gateway = Arc::new(HttpDriverApiGateway::new(UpstreamOptions::from_env()?));
    let (tiles, tile_options) = map_tiles::from_env()?;

    let metrics = Arc::new(Metrics::new());
    let auth = Arc::new(AuthService::with_authenticator(
        sessions,
        authenticator,
        options.session_ttl,
    ));
    let shift = Arc::new(ShiftService::new(gateway));
    let rate_limiter = Arc::new(RateLimiter::new(
        options.auth_rate_limit,
        options.auth_identity_rate_limit,
    ));

    let dispatcher = RpcDispatcher::new(
        Arc::clone(&auth),
        shift,
        Arc::clone(&tiles),
        tile_options.max_zoom,
        Arc::clone(&metrics),
        Arc::clone(&rate_limiter),
        options.session_debug_enabled,
    );
    let dispatcher = Arc::new(match vehicle_service {
        Some(service) => dispatcher.with_vehicles(service),
        None => dispatcher,
    });

    QuicServer {
        options,
        dispatcher,
        metrics,
        auth,
        rate_limiter,
        tiles,
    }
    .run(shutdown)
    .await
}
