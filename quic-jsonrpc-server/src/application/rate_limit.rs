//! Token-bucket rate limiter for credential-bearing methods (`login`, `rpc.resume`).
//!
//! The server proxies logins to the upstream driver API; without a limiter a single client could
//! brute-force passwords (or enumerate session ids) at full network speed.
//!
//! Two tiers are enforced independently:
//! * **per client IP** — a coarse flood guard. Mobile clients sit behind carrier-grade NAT and
//!   share IPs, so this tier is deliberately permissive;
//! * **per identity** — the phone number being logged in / the session id being resumed. This
//!   is the tier that actually stops password guessing against one account regardless of how
//!   many IPs the attacker rotates through.
//!
//! Stale buckets are evicted by [`RateLimiter::cleanup`].

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub struct RateLimitConfig {
    /// Sustained rate: how many requests per minute refill the bucket.
    pub per_minute: u32,
    /// Bucket capacity: how many requests may arrive back-to-back before throttling kicks in.
    pub burst: u32,
}

impl RateLimitConfig {
    pub const fn disabled() -> Self {
        Self {
            per_minute: 0,
            burst: 0,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.per_minute > 0 && self.burst > 0
    }
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// One tier: a config plus its buckets.
struct Tier {
    config: RateLimitConfig,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl Tier {
    fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn check_at(&self, key: &str, now: Instant) -> Result<(), Duration> {
        if !self.config.is_enabled() {
            return Ok(());
        }
        let refill_per_sec = f64::from(self.config.per_minute) / 60.0;
        let capacity = f64::from(self.config.burst);

        let Ok(mut buckets) = self.buckets.lock() else {
            return Ok(());
        };
        let bucket = buckets.entry(key.to_string()).or_insert(Bucket {
            tokens: capacity,
            last_refill: now,
        });

        let elapsed = now.saturating_duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill_per_sec).min(capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let missing = 1.0 - bucket.tokens;
            Err(Duration::from_secs_f64((missing / refill_per_sec).max(0.001)))
        }
    }

    fn cleanup(&self, now: Instant, idle_for: Duration) -> usize {
        let Ok(mut buckets) = self.buckets.lock() else {
            return 0;
        };
        let before = buckets.len();
        buckets.retain(|_, b| now.saturating_duration_since(b.last_refill) < idle_for);
        before - buckets.len()
    }
}

pub struct RateLimiter {
    per_ip: Tier,
    per_identity: Tier,
}

impl RateLimiter {
    pub fn new(per_ip: RateLimitConfig, per_identity: RateLimitConfig) -> Self {
        Self {
            per_ip: Tier::new(per_ip),
            per_identity: Tier::new(per_identity),
        }
    }

    pub fn ip_config(&self) -> RateLimitConfig {
        self.per_ip.config
    }

    pub fn identity_config(&self) -> RateLimitConfig {
        self.per_identity.config
    }

    /// Consumes one token from the IP bucket and, when `identity` is known, one from the
    /// identity bucket. Returns `Err(retry_after)` as soon as either bucket is empty.
    ///
    /// The identity tier is checked first so that an attacker hammering one account does not
    /// also drain the shared IP bucket for the legitimate users behind the same NAT.
    pub fn check(&self, ip: IpAddr, identity: Option<&str>) -> Result<(), Duration> {
        self.check_at(ip, identity, Instant::now())
    }

    fn check_at(&self, ip: IpAddr, identity: Option<&str>, now: Instant) -> Result<(), Duration> {
        if let Some(identity) = identity.map(str::trim).filter(|s| !s.is_empty()) {
            self.per_identity.check_at(identity, now)?;
        }
        self.per_ip.check_at(&ip.to_string(), now)
    }

    /// Drops buckets that have been idle for longer than `idle_for`. Returns how many.
    pub fn cleanup(&self, idle_for: Duration) -> usize {
        let now = Instant::now();
        self.per_ip.cleanup(now, idle_for) + self.per_identity.cleanup(now, idle_for)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    const IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    const OTHER_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

    fn ip_only(per_minute: u32, burst: u32) -> RateLimiter {
        RateLimiter::new(RateLimitConfig { per_minute, burst }, RateLimitConfig::disabled())
    }

    #[test]
    fn allows_burst_then_throttles() {
        let limiter = ip_only(60, 3);
        let t0 = Instant::now();
        assert!(limiter.check_at(IP, None, t0).is_ok());
        assert!(limiter.check_at(IP, None, t0).is_ok());
        assert!(limiter.check_at(IP, None, t0).is_ok());
        let retry = limiter
            .check_at(IP, None, t0)
            .expect_err("4th request must be throttled");
        assert!(retry <= Duration::from_secs(1));
        // One token per second refills.
        assert!(limiter.check_at(IP, None, t0 + Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn buckets_are_per_ip() {
        let limiter = ip_only(60, 1);
        let t0 = Instant::now();
        assert!(limiter.check_at(IP, None, t0).is_ok());
        assert!(limiter.check_at(IP, None, t0).is_err());
        assert!(limiter.check_at(OTHER_IP, None, t0).is_ok());
    }

    #[test]
    fn identity_tier_spans_ips_and_does_not_drain_ip_bucket() {
        let limiter = RateLimiter::new(
            RateLimitConfig {
                per_minute: 60,
                burst: 100,
            },
            RateLimitConfig {
                per_minute: 60,
                burst: 1,
            },
        );
        let t0 = Instant::now();
        assert!(limiter.check_at(IP, Some("+380991112233"), t0).is_ok());
        // Same account from another IP → still throttled.
        assert!(limiter.check_at(OTHER_IP, Some("+380991112233"), t0).is_err());
        // Another account behind the same NAT is unaffected.
        assert!(limiter.check_at(IP, Some("+380997770000"), t0).is_ok());
        // Blank identity is ignored (falls through to the IP tier only).
        assert!(limiter.check_at(IP, Some("  "), t0).is_ok());
    }

    #[test]
    fn disabled_config_never_throttles() {
        let limiter = RateLimiter::new(RateLimitConfig::disabled(), RateLimitConfig::disabled());
        for _ in 0..1000 {
            assert!(limiter.check(IP, Some("x")).is_ok());
        }
    }

    #[test]
    fn cleanup_removes_idle_buckets_in_both_tiers() {
        let limiter = RateLimiter::new(
            RateLimitConfig {
                per_minute: 60,
                burst: 1,
            },
            RateLimitConfig {
                per_minute: 60,
                burst: 1,
            },
        );
        let _ = limiter.check(IP, Some("id"));
        assert_eq!(limiter.cleanup(Duration::from_secs(3600)), 0);
        assert_eq!(limiter.cleanup(Duration::ZERO), 2);
    }
}
