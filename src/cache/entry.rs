//! Cache entry definitions and state management helpers.

// self
use crate::{
	_prelude::*,
	cache::state::{CachePayload, CacheState},
};

/// Represents a cached JWKS entry for a tenant/provider pair.
#[derive(Clone, Debug)]
pub struct CacheEntry {
	tenant_id: Arc<str>,
	provider_id: Arc<str>,
	state: CacheState,
}
impl CacheEntry {
	/// Create a new empty cache entry.
	pub fn new(tenant_id: impl Into<Arc<str>>, provider_id: impl Into<Arc<str>>) -> Self {
		Self {
			tenant_id: tenant_id.into(),
			provider_id: provider_id.into(),
			state: CacheState::Empty,
		}
	}

	/// Tenant identifier for this cache entry.
	pub fn tenant_id(&self) -> &str {
		&self.tenant_id
	}

	/// Provider identifier for this cache entry.
	pub fn provider_id(&self) -> &str {
		&self.provider_id
	}

	/// Inspect the current cache state.
	pub fn state(&self) -> &CacheState {
		&self.state
	}

	/// Attempt to begin an initial load; returns false when already loading or ready.
	pub fn begin_load(&mut self) -> bool {
		match self.state {
			CacheState::Empty => {
				self.state = CacheState::Loading;

				true
			},
			_ => false,
		}
	}

	/// Record a successful load or refresh, updating state to `Ready`.
	pub fn load_success(&mut self, mut payload: CachePayload) {
		payload.reset_failures();
		self.state = CacheState::Ready(payload);
	}

	/// Attempt to transition into refreshing state when scheduled refresh is due.
	pub fn begin_refresh(&mut self, now: Instant) -> bool {
		match &mut self.state {
			CacheState::Ready(payload) =>
				if now >= payload.next_refresh_at {
					let next = payload.clone();
					self.state = CacheState::Refreshing(next);

					true
				} else {
					false
				},
			CacheState::Refreshing(_) | CacheState::Loading | CacheState::Empty => false,
		}
	}

	/// Record a successful refresh.
	pub fn refresh_success(&mut self, mut payload: CachePayload) {
		payload.reset_failures();
		self.state = CacheState::Ready(payload);
	}

	/// Record a refresh failure and decide whether stale data can remain active.
	///
	/// When a backoff is provided the next refresh instant is shifted forward
	/// by that duration, effectively treating it as a cooldown on top of the
	/// previously scheduled refresh window.
	pub fn refresh_failure(&mut self, now: Instant, next_backoff: Option<Duration>) {
		self.state = match std::mem::replace(&mut self.state, CacheState::Empty) {
			CacheState::Refreshing(mut payload) => {
				payload.bump_error(next_backoff);

				if let Some(delay) = next_backoff {
					payload.next_refresh_at = now + delay;
				}

				if payload.can_serve_stale(now) {
					CacheState::Ready(payload)
				} else {
					CacheState::Empty
				}
			},
			state => state,
		};
	}

	/// Invalidate the cached payload, returning to Empty state.
	pub fn invalidate(&mut self) {
		self.state = CacheState::Empty;
	}

	/// Retrieve a clone of the cached payload if present.
	pub fn snapshot(&self) -> Option<CachePayload> {
		self.state.payload().cloned()
	}
}

#[cfg(test)]
mod tests {
	// crates.io
	use http::{Request, Response, StatusCode};
	use http_cache_semantics::CachePolicy;
	use jsonwebtoken::jwk::JwkSet;
	// self
	use super::*;

	fn sample_payload(now: Instant) -> CachePayload {
		let request = Request::builder()
			.method("GET")
			.uri("https://example.com/.well-known/jwks.json")
			.body(())
			.expect("request");
		let response = Response::builder().status(StatusCode::OK).body(()).expect("response");
		let policy = CachePolicy::new(&request, &response);

		CachePayload {
			jwks: Arc::new(JwkSet { keys: Vec::new() }),
			policy,
			etag: Some("v1".to_string()),
			last_modified: None,
			last_refresh_at: Utc::now(),
			expires_at: now + Duration::from_secs(60),
			next_refresh_at: now + Duration::from_secs(30),
			stale_deadline: Some(now + Duration::from_secs(120)),
			retry_backoff: None,
			error_count: 0,
		}
	}

	#[test]
	fn load_success_moves_entry_into_ready_state() {
		let mut entry = CacheEntry::new("tenant", "provider");

		assert!(matches!(entry.state(), CacheState::Empty));
		assert!(entry.begin_load());

		let now = Instant::now();
		let payload = sample_payload(now);

		entry.load_success(payload.clone());

		match entry.state() {
			CacheState::Ready(meta) => {
				assert_eq!(meta.etag.as_deref(), Some("v1"));
				assert_eq!(meta.error_count, 0);
				assert!(meta.expires_at > now);
			},
			other => panic!("expected Ready state, got {:?}", other),
		}
	}

	#[test]
	fn begin_refresh_moves_ready_to_refreshing() {
		let mut entry = CacheEntry::new("tenant", "provider");

		entry.begin_load();

		let now = Instant::now();

		entry.load_success(sample_payload(now));

		assert!(entry.begin_refresh(now + Duration::from_secs(31)));
		matches!(entry.state(), CacheState::Refreshing(_));
	}

	#[test]
	fn refresh_failure_without_stale_deadline_clears_entry() {
		let mut entry = CacheEntry::new("tenant", "provider");

		entry.begin_load();

		let now = Instant::now();
		let mut payload = sample_payload(now);

		payload.stale_deadline = None;
		entry.load_success(payload);

		assert!(entry.begin_refresh(now + Duration::from_secs(31)));

		entry.refresh_failure(now + Duration::from_secs(90), None);

		assert!(matches!(entry.state(), CacheState::Empty));
	}
}
