//! Authentication throttling and peer-identity attachment.
//!
//! The limiter is a bounded sliding window keyed by the transport peer
//! address. It never delays a successful authentication and never retains
//! unbounded per-source history. Once the source table is full, further
//! sources are left untracked rather than blocked, so the ceiling can never be
//! used to lock out a legitimate new peer.

use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    time::{Duration, Instant},
};

use eggtunnel_proto::{BoundedDiagnostic, ErrorMessage, Message};
use tokio::io::AsyncWrite;

use crate::common::{Counters, TunnelError};

use crate::wire_io::write_message;

pub(super) const AUTH_FAILURES_PER_SOURCE: usize = 10;
pub(super) const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);
pub(super) const MAX_AUTH_SOURCES: usize = 1024;
pub(super) const AUTH_FAILURE_DELAY: Duration = Duration::from_millis(100);
const AUTH_FAILURE_WRITE_BUDGET: Duration = Duration::from_secs(1);
/// `Error` code emitted to a peer whose bearer token did not verify.
pub(super) const AUTH_FAILURE_CODE: u16 = 4;

/// Bounded sliding-window limiter keyed by the transport peer address. It
/// retains no unbounded per-source history and never delays successful
/// authentication.
pub(super) struct AuthFailureLimiter {
    pub(super) failures: std::sync::Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
    threshold: usize,
    window: Duration,
    max_sources: usize,
}

impl AuthFailureLimiter {
    pub(super) fn new(threshold: usize, window: Duration, max_sources: usize) -> Self {
        Self {
            failures: std::sync::Mutex::new(HashMap::new()),
            threshold,
            window,
            max_sources,
        }
    }

    pub(super) fn is_blocked(&self, source: IpAddr) -> bool {
        self.is_blocked_at(source, Instant::now())
    }

    pub(super) fn is_blocked_at(&self, source: IpAddr, now: Instant) -> bool {
        let mut sources = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        self.prune(&mut sources, now);
        // A saturated table is a memory ceiling, not a verdict: a source with
        // no recorded failure is never blocked, so the table cannot be filled
        // with spoofed addresses to lock out legitimate new peers. Such a
        // source simply goes untracked until the window prunes an entry.
        sources
            .get(&source)
            .is_some_and(|failures| failures.len() >= self.threshold)
    }

    pub(super) fn record_failure(&self, source: IpAddr) {
        self.record_failure_at(source, Instant::now());
    }

    pub(super) fn record_failure_at(&self, source: IpAddr, now: Instant) {
        let mut sources = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        self.prune(&mut sources, now);
        if !sources.contains_key(&source) && sources.len() >= self.max_sources {
            tracing::debug!(
                category = "auth_source_table_saturated",
                tracked_sources = sources.len(),
                "authentication failure is not tracked",
            );
            return;
        }
        sources.entry(source).or_default().push_back(now);
        // Bound the in-window burst per source to the blocking threshold:
        // `is_blocked` only needs `threshold` entries to verdict, so any
        // excess is dropped from the front. This keeps the documented
        // "never retains unbounded per-source history" true even for a
        // single-IP flood within `AUTH_FAILURE_WINDOW`.
        if let Some(deque) = sources.get_mut(&source) {
            while deque.len() > self.threshold {
                deque.pop_front();
            }
        }
    }

    pub(super) fn prune(&self, sources: &mut HashMap<IpAddr, VecDeque<Instant>>, now: Instant) {
        sources.retain(|_, failures| {
            while failures
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) >= self.window)
            {
                failures.pop_front();
            }
            !failures.is_empty()
        });
    }
}

/// Record a failed bearer-token verification, release the caller's admission
/// slots, apply the fixed throttle delay, and answer the peer.
///
/// Every refused peer leaves through this one function — blocked sources and
/// bad tokens alike — so a prober cannot separate blocklist membership from
/// token validity by the presence or absence of the frame. The delay is a
/// deliberate constant-cost response: it is applied only to refused peers and
/// never to a successful authentication.
pub(super) async fn reject_authentication(
    stream: &mut (impl AsyncWrite + Unpin),
    source: IpAddr,
    auth_failures: &AuthFailureLimiter,
    counters: &Counters,
) -> Result<(), TunnelError> {
    tracing::debug!(category = "authentication", "client authentication refused");
    auth_failures.record_failure(source);
    tokio::time::sleep(AUTH_FAILURE_DELAY).await;
    counters
        .rejected
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let failure = Message::Error(ErrorMessage {
        code: AUTH_FAILURE_CODE,
        diagnostic: BoundedDiagnostic::new("authentication failed")?,
    });
    let _ = tokio::time::timeout(AUTH_FAILURE_WRITE_BUDGET, write_message(stream, &failure)).await;
    Err(TunnelError::Authentication)
}
