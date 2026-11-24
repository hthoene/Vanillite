//! Connection limits (SEC-18) and per-connection rate limiting (SEC-16).

use crate::config::RateLimit;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use vanillite_protocol::Bucket;

/// Counts open connections in total and pending (not yet playing) connections per IP.
#[derive(Debug)]
pub struct ConnectionLimiter {
    max_total: usize,
    max_per_ip: u32,
    state: Mutex<(usize, HashMap<IpAddr, u32>)>,
}

impl ConnectionLimiter {
    /// A limiter.
    pub fn new(max_total: usize, max_per_ip: u32) -> Arc<Self> {
        Arc::new(Self {
            max_total,
            max_per_ip,
            state: Mutex::new((0, HashMap::new())),
        })
    }

    /// Admits a connection or refuses it.
    pub fn admit(self: &Arc<Self>, ip: IpAddr) -> Option<ConnectionGuard> {
        let mut g = self.state.lock().ok()?;
        let (total, per_ip) = &mut *g;
        if *total >= self.max_total {
            return None;
        }
        let count = per_ip.entry(ip).or_insert(0);
        if *count >= self.max_per_ip {
            return None;
        }
        *count = count.saturating_add(1);
        *total = total.saturating_add(1);
        Some(ConnectionGuard {
            limiter: Arc::clone(self),
            ip,
            pending: true,
        })
    }
}

/// Releases its slots when dropped.
#[derive(Debug)]
pub struct ConnectionGuard {
    limiter: Arc<ConnectionLimiter>,
    ip: IpAddr,
    pending: bool,
}

impl ConnectionGuard {
    /// The connection reached play: it no longer counts against the per-IP pending limit.
    pub fn joined(&mut self) {
        if self.pending {
            self.pending = false;
            self.release_ip();
        }
    }

    fn release_ip(&self) {
        if let Ok(mut g) = self.limiter.state.lock() {
            let per_ip = &mut g.1;
            if let Some(c) = per_ip.get_mut(&self.ip) {
                *c = c.saturating_sub(1);
                if *c == 0 {
                    per_ip.remove(&self.ip);
                }
            }
        }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if self.pending {
            self.release_ip();
        }
        if let Ok(mut g) = self.limiter.state.lock() {
            g.0 = g.0.saturating_sub(1);
        }
    }
}

/// Token buckets of one connection.
#[derive(Debug)]
pub struct RateLimiter {
    limits: [RateLimit; Bucket::COUNT],
    tokens: [f32; Bucket::COUNT],
    last: Instant,
}

impl RateLimiter {
    /// Full buckets.
    pub fn new(limits: [RateLimit; Bucket::COUNT]) -> Self {
        Self {
            limits,
            tokens: limits.map(|l| l.burst),
            last: Instant::now(),
        }
    }

    /// Takes `cost` tokens from `bucket`; false means the client exceeded the limit.
    pub fn take(&mut self, bucket: Bucket, cost: u16) -> bool {
        let now = Instant::now();
        let dt = now.saturating_duration_since(self.last).as_secs_f32();
        self.last = now;
        for (t, l) in self.tokens.iter_mut().zip(self.limits.iter()) {
            *t = (*t + l.rate * dt).min(l.burst);
        }
        match self.tokens.get_mut(bucket.index()) {
            Some(t) if *t >= f32::from(cost) => {
                *t -= f32::from(cost);
                true
            }
            _ => false,
        }
    }
}
