//! Serve tiles from a byte-bounded in-memory cache, fetching misses from the upstream
//! [`TileSource`] with single-flight (concurrent requests for the same tile share one upstream
//! call).

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, OnceCell};
use tracing::debug;

use crate::domain::{Tile, TileCoord, TileResult, TileSource};

/// Cache sizing / freshness policy.
#[derive(Clone, Copy, Debug)]
pub struct TileCacheConfig {
    /// Upper bound on the summed size of cached tile bytes. `0` disables caching.
    pub max_bytes: usize,
    /// How long a cached tile stays valid.
    pub ttl: Duration,
}

/// Outcome of a tile lookup.
#[derive(Clone, Debug)]
pub struct TileLookup {
    pub tile: Arc<Tile>,
    /// `true` when served from the in-memory cache without touching the upstream.
    pub from_cache: bool,
}

struct CacheEntry {
    tile: Arc<Tile>,
    inserted_at: Instant,
    last_used: Instant,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<TileCoord, CacheEntry>,
    total_bytes: usize,
}

impl CacheState {
    fn get(&mut self, coord: &TileCoord, ttl: Duration, now: Instant) -> Option<Arc<Tile>> {
        let expired = now.duration_since(self.entries.get(coord)?.inserted_at) >= ttl;
        if expired {
            self.remove(coord);
            return None;
        }
        let entry = self.entries.get_mut(coord)?;
        entry.last_used = now;
        Some(Arc::clone(&entry.tile))
    }

    fn remove(&mut self, coord: &TileCoord) {
        if let Some(entry) = self.entries.remove(coord) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.tile.size_bytes());
        }
    }

    /// Inserts `tile`, evicting least-recently-used entries until the budget is respected.
    /// A tile larger than the whole budget is simply not cached.
    fn insert(&mut self, coord: TileCoord, tile: Arc<Tile>, max_bytes: usize, now: Instant) {
        let size = tile.size_bytes();
        if size > max_bytes {
            return;
        }
        self.remove(&coord);
        while self.total_bytes + size > max_bytes {
            let Some(victim) = self.entries.iter().min_by_key(|(_, e)| e.last_used).map(|(c, _)| *c) else {
                break;
            };
            self.remove(&victim);
        }
        self.total_bytes += size;
        self.entries.insert(
            coord,
            CacheEntry {
                tile,
                inserted_at: now,
                last_used: now,
            },
        );
    }

    fn evict_expired(&mut self, ttl: Duration, now: Instant) -> usize {
        let expired: Vec<TileCoord> = self
            .entries
            .iter()
            .filter(|(_, e)| now.duration_since(e.inserted_at) >= ttl)
            .map(|(c, _)| *c)
            .collect();
        for coord in &expired {
            self.remove(coord);
        }
        expired.len()
    }
}

type InFlight = Arc<OnceCell<TileResult<Arc<Tile>>>>;

pub struct TileService {
    source: Arc<dyn TileSource>,
    config: TileCacheConfig,
    cache: Mutex<CacheState>,
    in_flight: Mutex<HashMap<TileCoord, InFlight>>,
}

impl TileService {
    pub fn new(source: Arc<dyn TileSource>, config: TileCacheConfig) -> Self {
        Self {
            source,
            config,
            cache: Mutex::new(CacheState::default()),
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> TileCacheConfig {
        self.config
    }

    pub async fn get(&self, coord: TileCoord) -> TileResult<TileLookup> {
        let caching = self.config.max_bytes > 0;

        if caching && let Some(tile) = self.cache.lock().await.get(&coord, self.config.ttl, Instant::now()) {
            return Ok(TileLookup { tile, from_cache: true });
        }

        // Single-flight: the first caller for a coord owns the upstream fetch, the others await
        // the same cell.
        let cell = {
            let mut in_flight = self.in_flight.lock().await;
            Arc::clone(in_flight.entry(coord).or_default())
        };

        let result = cell
            .get_or_init(|| async {
                let fetched = self.source.fetch(coord).await.map(Arc::new);
                match &fetched {
                    Ok(tile) => {
                        debug!("tile {coord} fetched from upstream: {} bytes", tile.size_bytes());
                        if caching {
                            self.cache.lock().await.insert(
                                coord,
                                Arc::clone(tile),
                                self.config.max_bytes,
                                Instant::now(),
                            );
                        }
                    }
                    Err(err) => debug!("tile {coord} upstream fetch failed: {err}"),
                }
                fetched
            })
            .await
            .clone();

        // Whoever observes the completed cell may clear the slot; a later request for the same
        // coord then goes through the cache (hit) or starts a fresh fetch (after an error).
        let mut in_flight = self.in_flight.lock().await;
        if in_flight.get(&coord).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
            in_flight.remove(&coord);
        }

        result.map(|tile| TileLookup {
            tile,
            from_cache: false,
        })
    }

    /// Drops expired entries; returns how many were removed. Meant for a periodic tick.
    pub async fn evict_expired(&self) -> usize {
        self.cache.lock().await.evict_expired(self.config.ttl, Instant::now())
    }

    /// Current cache footprint: `(entries, bytes)`.
    pub async fn cache_usage(&self) -> (usize, usize) {
        let cache = self.cache.lock().await;
        (cache.entries.len(), cache.total_bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use crate::domain::TileError;

    use super::*;

    struct CountingSource {
        calls: AtomicUsize,
        fail: bool,
        delay: Duration,
    }

    #[async_trait]
    impl TileSource for CountingSource {
        async fn fetch(&self, coord: TileCoord) -> TileResult<Tile> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            if self.fail {
                return Err(TileError::UpstreamStatus(502));
            }
            Ok(Tile {
                content_type: "image/png".into(),
                bytes: format!("tile-{coord}").into_bytes(),
                etag: None,
            })
        }
    }

    fn service(fail: bool, max_bytes: usize, ttl: Duration) -> (Arc<TileService>, Arc<CountingSource>) {
        let source = Arc::new(CountingSource {
            calls: AtomicUsize::new(0),
            fail,
            delay: Duration::from_millis(20),
        });
        let svc = Arc::new(TileService::new(source.clone(), TileCacheConfig { max_bytes, ttl }));
        (svc, source)
    }

    #[tokio::test]
    async fn second_lookup_is_served_from_cache() {
        let (svc, source) = service(false, 1 << 20, Duration::from_secs(60));
        let coord = TileCoord::new(3, 1, 2, 19).unwrap();
        let first = svc.get(coord).await.unwrap();
        let second = svc.get(coord).await.unwrap();
        assert!(!first.from_cache);
        assert!(second.from_cache);
        assert_eq!(first.tile, second.tile);
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_misses_share_one_upstream_call() {
        let (svc, source) = service(false, 1 << 20, Duration::from_secs(60));
        let coord = TileCoord::new(5, 4, 4, 19).unwrap();
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let svc = Arc::clone(&svc);
                tokio::spawn(async move { svc.get(coord).await })
            })
            .collect();
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn errors_are_not_cached() {
        let (svc, source) = service(true, 1 << 20, Duration::from_secs(60));
        let coord = TileCoord::new(1, 0, 0, 19).unwrap();
        assert_eq!(svc.get(coord).await.unwrap_err(), TileError::UpstreamStatus(502));
        assert!(svc.get(coord).await.is_err());
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cache_respects_byte_budget_with_lru_eviction() {
        // Each tile is "tile-z/x/y" ≈ 10 bytes; budget fits two.
        let (svc, _) = service(false, 22, Duration::from_secs(60));
        let a = TileCoord::new(4, 1, 1, 19).unwrap();
        let b = TileCoord::new(4, 2, 2, 19).unwrap();
        let c = TileCoord::new(4, 3, 3, 19).unwrap();
        svc.get(a).await.unwrap();
        svc.get(b).await.unwrap();
        svc.get(a).await.unwrap(); // touch a → b is now LRU
        svc.get(c).await.unwrap(); // evicts b
        let (entries, bytes) = svc.cache_usage().await;
        assert_eq!(entries, 2);
        assert!(bytes <= 22);
        assert!(svc.get(a).await.unwrap().from_cache);
        assert!(!svc.get(b).await.unwrap().from_cache);
    }

    #[tokio::test]
    async fn expired_entries_are_refetched() {
        let (svc, source) = service(false, 1 << 20, Duration::from_millis(30));
        let coord = TileCoord::new(2, 1, 1, 19).unwrap();
        svc.get(coord).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(svc.evict_expired().await, 1);
        assert!(!svc.get(coord).await.unwrap().from_cache);
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn zero_budget_disables_caching() {
        let (svc, source) = service(false, 0, Duration::from_secs(60));
        let coord = TileCoord::new(2, 1, 1, 19).unwrap();
        svc.get(coord).await.unwrap();
        svc.get(coord).await.unwrap();
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    }
}
