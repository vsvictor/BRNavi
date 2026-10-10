//! Process-wide counters exposed through `rpc.health` and periodic log lines.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug)]
pub enum ConnectionCloseCause {
    PeerOrApplicationClosed,
    LocallyClosed,
    TimedOut,
    TransportError,
}

#[derive(Default)]
pub struct Metrics {
    started_at: Option<Instant>,
    pub accepted_connections: AtomicU64,
    pub closed_connections: AtomicU64,
    pub opened_streams: AtomicU64,
    pub processed_requests: AtomicU64,
    pub sent_responses: AtomicU64,
    pub stream_errors: AtomicU64,
    pub tile_cache_hits: AtomicU64,
    pub tile_cache_misses: AtomicU64,
    pub tile_upstream_errors: AtomicU64,
    pub handshake_success_count: AtomicU64,
    pub handshake_success_total_ms: AtomicU64,
    pub handshake_success_max_ms: AtomicU64,
    pub handshake_failures: AtomicU64,
    pub disconnect_peer_or_application_closed: AtomicU64,
    pub disconnect_locally_closed: AtomicU64,
    pub disconnect_timed_out: AtomicU64,
    pub disconnect_transport_error: AtomicU64,
    next_connection_id: AtomicU64,
    next_stream_id: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            started_at: Some(Instant::now()),
            next_connection_id: AtomicU64::new(1),
            next_stream_id: AtomicU64::new(1),
            ..Self::default()
        }
    }

    pub fn next_connection_id(&self) -> u64 {
        self.next_connection_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn next_stream_id(&self) -> u64 {
        self.next_stream_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn record_handshake_success(&self, elapsed: Duration) {
        let elapsed_ms = elapsed.as_millis().min(u64::MAX as u128) as u64;
        self.handshake_success_count.fetch_add(1, Ordering::Relaxed);
        self.handshake_success_total_ms.fetch_add(elapsed_ms, Ordering::Relaxed);

        let mut current_max = self.handshake_success_max_ms.load(Ordering::Relaxed);
        while elapsed_ms > current_max {
            match self.handshake_success_max_ms.compare_exchange_weak(
                current_max,
                elapsed_ms,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current_max = observed,
            }
        }
    }

    pub fn record_handshake_failure(&self) {
        self.handshake_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_disconnect(&self, cause: ConnectionCloseCause) {
        match cause {
            ConnectionCloseCause::PeerOrApplicationClosed => {
                self.disconnect_peer_or_application_closed
                    .fetch_add(1, Ordering::Relaxed);
            }
            ConnectionCloseCause::LocallyClosed => {
                self.disconnect_locally_closed.fetch_add(1, Ordering::Relaxed);
            }
            ConnectionCloseCause::TimedOut => {
                self.disconnect_timed_out.fetch_add(1, Ordering::Relaxed);
            }
            ConnectionCloseCause::TransportError => {
                self.disconnect_transport_error.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn snapshot(&self) -> Value {
        let uptime_ms = self.started_at.map(|s| s.elapsed().as_millis() as u64).unwrap_or(0);
        let handshake_successes = self.handshake_success_count.load(Ordering::Relaxed);
        let handshake_total_ms = self.handshake_success_total_ms.load(Ordering::Relaxed);
        let handshake_avg_ms = handshake_total_ms.checked_div(handshake_successes).unwrap_or(0);
        json!({
            "uptime_ms": uptime_ms,
            "connections": {
                "accepted": self.accepted_connections.load(Ordering::Relaxed),
                "closed": self.closed_connections.load(Ordering::Relaxed),
            },
            "streams": {
                "opened": self.opened_streams.load(Ordering::Relaxed),
                "errors": self.stream_errors.load(Ordering::Relaxed),
            },
            "rpc": {
                "requests": self.processed_requests.load(Ordering::Relaxed),
                "responses": self.sent_responses.load(Ordering::Relaxed),
            },
            "tiles": {
                "cache_hits": self.tile_cache_hits.load(Ordering::Relaxed),
                "cache_misses": self.tile_cache_misses.load(Ordering::Relaxed),
                "upstream_errors": self.tile_upstream_errors.load(Ordering::Relaxed),
            },
            "quic": {
                "handshake": {
                    "success": handshake_successes,
                    "failed": self.handshake_failures.load(Ordering::Relaxed),
                    "latency_avg_ms": handshake_avg_ms,
                    "latency_max_ms": self.handshake_success_max_ms.load(Ordering::Relaxed),
                },
                "disconnects": {
                    "peer_or_application_closed": self
                        .disconnect_peer_or_application_closed
                        .load(Ordering::Relaxed),
                    "locally_closed": self.disconnect_locally_closed.load(Ordering::Relaxed),
                    "timed_out": self.disconnect_timed_out.load(Ordering::Relaxed),
                    "transport_error": self.disconnect_transport_error.load(Ordering::Relaxed),
                }
            }
        })
    }
}
