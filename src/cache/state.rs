//! Cache state machine modelling JWKS lifecycle transitions.

// crates.io
use http_cache_semantics::CachePolicy;
use jsonwebtoken::jwk::JwkSet;
// self
use crate::_prelude::*;

/// Metadata captured for a cached JWKS payload.
#[derive(Clone, Debug)]
pub struct CachePayload {
	/// JWKS document retained for the provider.
	pub jwks: Arc<JwkSet>,
	/// HTTP cache policy derived from the last response.
	pub policy: CachePolicy,
	/// Strong or weak validator supplied by the origin.
	pub etag: Option<String>,
	/// Last-Modified timestamp advertised by the origin.
	pub last_modified: Option<DateTime<Utc>>,
	/// UTC timestamp when the payload was most recently refreshed.
	pub last_refresh_at: DateTime<Utc>,
	/// Monotonic deadline after which the payload is considered expired.
	pub expires_at: Instant,
	/// Monotonic schedule for the next proactive refresh.
	///
	/// On refresh failures this is also repurposed as the cooldown deadline by
	/// adding the computed retry backoff to the current `Instant`.
	pub next_refresh_at: Instant,
	/// Optional window permitting stale serving past expiry.
	pub stale_deadline: Option<Instant>,
	/// Exponential backoff duration before retrying a failed refresh.
	///
	/// This stores the most recent backoff duration; the cache manager combines
	/// it with `next_refresh_at` to produce the absolute retry instant.
	pub retry_backoff: Option<Duration>,
	/// Count of consecutive refresh errors.
	pub error_count: u32,
}
impl CachePayload {
	/// Whether the payload has exceeded its freshness window.
	pub fn is_expired(&self, now: Instant) -> bool {
		now >= self.expires_at
	}

	/// Whether stale serving is still permitted at the given time.
	pub fn can_serve_stale(&self, now: Instant) -> bool {
		self.stale_deadline.map(|deadline| now <= deadline).unwrap_or(false)
	}

	/// Update retry bookkeeping after a failed refresh.
	pub fn bump_error(&mut self, backoff: Option<Duration>) {
		self.error_count = self.error_count.saturating_add(1);
		self.retry_backoff = backoff;
	}

	/// Reset failure bookkeeping after a successful refresh.
	pub fn reset_failures(&mut self) {
		self.error_count = 0;
		self.retry_backoff = None;
	}
}

/// Cache lifecycle states.
#[derive(Clone, Debug)]
pub enum CacheState {
	/// Cache has no payload and no work in progress.
	Empty,
	/// Initial fetch is underway and no payload is yet available.
	Loading,
	/// Fresh payload is ready for use.
	Ready(CachePayload),
	/// Payload is in use while a background refresh is running.
	Refreshing(CachePayload),
}
impl CacheState {
	/// Retrieve the current payload if available.
	pub fn payload(&self) -> Option<&CachePayload> {
		match self {
			CacheState::Ready(payload) | CacheState::Refreshing(payload) => Some(payload),
			_ => None,
		}
	}

	/// Mutable access to payload when state carries one.
	pub fn payload_mut(&mut self) -> Option<&mut CachePayload> {
		match self {
			CacheState::Ready(payload) | CacheState::Refreshing(payload) => Some(payload),
			_ => None,
		}
	}

	/// Whether the cached payload is immediately usable.
	pub fn is_usable(&self) -> bool {
		matches!(self, CacheState::Ready(_) | CacheState::Refreshing(_))
	}
}
