//! Cache manager handling JWKS retrieval and lifecycle.

// crates.io
use http::{
	HeaderName, HeaderValue, Request, Response,
	header::{ETAG, IF_NONE_MATCH, LAST_MODIFIED},
};
use http_cache_semantics::BeforeRequest;
#[cfg(feature = "redis")] use http_cache_semantics::CachePolicy;
use jsonwebtoken::jwk::JwkSet;
use rand::Rng;
use reqwest::{Client, redirect::Policy};
use tokio::{
	sync::{Mutex, RwLock},
	time,
};
// self
#[cfg(feature = "redis")] use crate::registry::PersistentSnapshot;
use crate::{
	_prelude::*,
	cache::{
		entry::CacheEntry,
		state::{CachePayload, CacheState},
	},
	http::{
		client::fetch_jwks,
		retry::{AttemptBudget, RetryExecutor},
		semantics::{Freshness, base_request, evaluate_freshness, evaluate_revalidation},
	},
	metrics::{self, ProviderMetrics},
	registry::IdentityProviderRegistration,
};

/// Coordinates fetching, caching, and background refresh for a registration.
///
/// Instances are scoped per tenant/provider pair; the single-flight guard only
/// serialises refresh work for that specific provider.
#[derive(Clone, Debug)]
pub struct CacheManager {
	registration: Arc<IdentityProviderRegistration>,
	client: Arc<Client>,
	entry: Arc<RwLock<CacheEntry>>,
	single_flight: Arc<Mutex<()>>,
	metrics: Arc<ProviderMetrics>,
}
impl CacheManager {
	/// Build a new cache manager with the default reqwest client.
	pub fn new(registration: IdentityProviderRegistration) -> Result<Self> {
		registration.validate()?;

		let client = Client::builder()
			.redirect(Policy::limited(10))
			.user_agent(format!("jwks-cache/{}", env!("CARGO_PKG_VERSION")))
			.connect_timeout(Duration::from_secs(5))
			.build()?;

		Ok(Self::with_parts(registration, client, ProviderMetrics::new()))
	}

	/// Build a cache manager using the supplied HTTP client (primarily for tests).
	pub fn with_client(registration: IdentityProviderRegistration, client: Client) -> Self {
		Self::with_parts(registration, client, ProviderMetrics::new())
	}

	fn with_parts(
		registration: IdentityProviderRegistration,
		client: Client,
		metrics: Arc<ProviderMetrics>,
	) -> Self {
		let tenant = registration.tenant_id.clone();
		let provider = registration.provider_id.clone();

		Self {
			registration: Arc::new(registration),
			client: Arc::new(client),
			entry: Arc::new(RwLock::new(CacheEntry::new(tenant, provider))),
			single_flight: Arc::new(Mutex::new(())),
			metrics,
		}
	}

	/// Access the per-provider metrics accumulator.
	pub fn metrics(&self) -> Arc<ProviderMetrics> {
		self.metrics.clone()
	}

	/// Capture the current cache state for status reporting.
	pub async fn snapshot(&self) -> CacheSnapshot {
		let captured_at = Instant::now();
		let captured_at_wallclock = Utc::now();
		let state = { self.entry.read().await.state().clone() };

		CacheSnapshot { captured_at, captured_at_wallclock, state }
	}

	#[cfg(feature = "redis")]
	/// Build a persistence payload capturing the current cache contents.
	pub async fn persistent_snapshot(&self) -> Result<Option<PersistentSnapshot>> {
		let snapshot = self.snapshot().await;
		let payload = match snapshot.state {
			CacheState::Ready(ref payload) | CacheState::Refreshing(ref payload) => payload.clone(),
			_ => return Ok(None),
		};
		let expires_at = match snapshot.to_datetime(payload.expires_at) {
			Some(dt) => dt,
			None => return Ok(None),
		};
		let jwks_json = serde_json::to_string(&*payload.jwks)?;
		let persisted_at = Utc::now();
		let snapshot = PersistentSnapshot {
			tenant_id: self.registration.tenant_id.clone(),
			provider_id: self.registration.provider_id.clone(),
			jwks_json,
			etag: payload.etag.clone(),
			last_modified: payload.last_modified,
			expires_at,
			persisted_at,
		};

		Ok(Some(snapshot))
	}

	#[cfg(feature = "redis")]
	/// Restore cache state from a previously persisted snapshot.
	pub async fn restore_snapshot(&self, snapshot: PersistentSnapshot) -> Result<()> {
		snapshot.validate(&self.registration)?;

		let PersistentSnapshot { jwks_json, etag, last_modified, expires_at, persisted_at, .. } =
			snapshot;
		let jwks: JwkSet = serde_json::from_str(&jwks_json)?;
		let jwks = Arc::new(jwks);
		let ttl = (expires_at - persisted_at)
			.to_std()
			.unwrap_or_default()
			.max(self.registration.min_ttl)
			.min(self.registration.max_ttl);
		let request = base_request(&self.registration)?;
		let mut response = Response::builder()
			.status(200)
			.header("cache-control", format!("public, max-age={}", ttl.as_secs()))
			.header("content-type", "application/json")
			.body(())
			.map_err(Error::from)?;

		if let Some(ref etag_value) = etag {
			let value = HeaderValue::from_str(etag_value).map_err(|err| Error::Validation {
				field: "etag",
				reason: format!("Invalid persisted ETag: {err}."),
			})?;

			response.headers_mut().insert(ETAG, value);
		}
		if let Some(ref last_modified_value) = last_modified {
			let http_date = httpdate::fmt_http_date((*last_modified_value).into());
			let value = HeaderValue::from_str(&http_date).map_err(|err| Error::Validation {
				field: "last_modified",
				reason: format!("Invalid persisted Last-Modified: {err}."),
			})?;

			response.headers_mut().insert(LAST_MODIFIED, value);
		}

		let policy = CachePolicy::new(&request, &response);
		let freshness = Freshness { ttl, policy };
		let now = Instant::now();
		let payload = self.build_payload(jwks, freshness, etag, last_modified, now, persisted_at);

		{
			let mut entry = self.entry.write().await;

			entry.load_success(payload.clone());
		}

		tracing::debug!(
			tenant = %self.registration.tenant_id,
			provider = %self.registration.provider_id,
			"restored cache entry from persistent snapshot"
		);

		Ok(())
	}

	/// Resolve JWKS for the registration, fetching upstream when necessary.
	#[tracing::instrument(
		skip(self, kid),
		fields(
			tenant = %self.registration.tenant_id,
			provider = %self.registration.provider_id,
			kid = kid.unwrap_or_default()
		)
	)]
	pub async fn resolve(&self, kid: Option<&str>) -> Result<Arc<JwkSet>> {
		loop {
			let snapshot = { self.entry.read().await.snapshot() };
			let now = Instant::now();

			match snapshot {
				None => {
					tracing::debug!("cache empty; performing initial fetch");

					match self.refresh_blocking(true).await? {
						RefreshOutcome::Updated { jwks, from_cache } => {
							if from_cache {
								self.observe_hit(false);
							} else {
								self.observe_miss();
							}

							return Ok(jwks);
						},
						RefreshOutcome::Stale(jwks) => {
							self.observe_hit(true);

							return Ok(jwks);
						},
					}
				},
				Some(payload) => {
					if !payload.is_expired(now) {
						let jwks = payload.jwks.clone();

						self.observe_hit(false);

						if now >= payload.next_refresh_at {
							self.schedule_background_refresh(now).await;
						}

						return Ok(jwks);
					}

					if payload.can_serve_stale(now) {
						// TODO(refactor): consolidate stale fallback with perform_fetch_with_retry
						// once the helper can orchestrate stale responses directly.
						match self.refresh_blocking(false).await {
							Ok(RefreshOutcome::Updated { jwks, from_cache }) => {
								if from_cache {
									self.observe_hit(false);
								} else {
									self.observe_miss();
								}

								return Ok(jwks);
							},
							Ok(RefreshOutcome::Stale(jwks)) => {
								self.observe_hit(true);

								return Ok(jwks);
							},
							Err(err) =>
								if payload.can_serve_stale(Instant::now()) {
									tracing::warn!(error = %err, "refresh failed, serving stale data");

									self.observe_hit(true);

									return Ok(payload.jwks.clone());
								} else {
									return Err(err);
								},
						}
					} else if let RefreshOutcome::Updated { jwks, from_cache } =
						self.refresh_blocking(true).await?
					{
						if from_cache {
							self.observe_hit(false);
						} else {
							self.observe_miss();
						}
						return Ok(jwks);
					}
				},
			}
		}
	}

	/// Trigger a manual refresh asynchronously; used by the control plane.
	#[tracing::instrument(
		skip(self),
		fields(tenant = %self.registration.tenant_id, provider = %self.registration.provider_id)
	)]
	pub async fn trigger_refresh(&self) -> Result<()> {
		let now = Instant::now();
		let action = {
			let mut entry = self.entry.write().await;

			match entry.state() {
				CacheState::Empty => {
					entry.begin_load();
					RefreshTrigger::Blocking
				},
				CacheState::Loading | CacheState::Refreshing(_) => RefreshTrigger::None,
				CacheState::Ready(_) =>
					if entry.begin_refresh(now) {
						RefreshTrigger::Background
					} else {
						RefreshTrigger::None
					},
			}
		};

		match action {
			RefreshTrigger::Background => {
				let manager = self.clone();

				tokio::spawn(async move {
					if let Err(err) = manager.refresh_blocking(true).await {
						tracing::warn!(error = %err, "manual refresh failed");
					}
				});
			},
			RefreshTrigger::Blocking => {
				self.refresh_blocking(true).await?;
			},
			RefreshTrigger::None => {},
		}

		Ok(())
	}

	#[tracing::instrument(
		skip(self),
		fields(tenant = %self.registration.tenant_id, provider = %self.registration.provider_id)
	)]
	async fn schedule_background_refresh(&self, now: Instant) {
		let should_spawn = {
			let mut entry = self.entry.write().await;

			entry.begin_refresh(now)
		};
		if should_spawn {
			let manager = self.clone();

			tokio::spawn(async move {
				if let Err(err) = manager.refresh_blocking(true).await {
					tracing::debug!(error = %err, "background refresh failed");
				}
			});
		}
	}

	#[tracing::instrument(
		skip(self, force_revalidation),
		fields(tenant = %self.registration.tenant_id, provider = %self.registration.provider_id, force_revalidation)
	)]
	async fn refresh_blocking(&self, force_revalidation: bool) -> Result<RefreshOutcome> {
		let _guard = self.single_flight.lock().await;
		let now = Instant::now();
		let (existing, mode) = {
			let mut entry = self.entry.write().await;
			let snapshot = entry.snapshot();
			let mode = if snapshot.is_some() {
				entry.begin_refresh(now);

				FetchMode::Refresh
			} else {
				entry.begin_load();

				FetchMode::Initial
			};

			(snapshot, mode)
		};

		match self.prepare_request(existing.as_ref(), force_revalidation)? {
			PreparedRequest::UseCached { jwks } =>
				Ok(RefreshOutcome::Updated { jwks, from_cache: true }),
			PreparedRequest::Send(request) =>
				self.perform_fetch_with_retry(*request, existing, mode, force_revalidation).await,
		}
	}

	fn prepare_request(
		&self,
		existing: Option<&CachePayload>,
		force_revalidation: bool,
	) -> Result<PreparedRequest> {
		let mut request = base_request(&self.registration)?;

		if let Some(payload) = existing {
			let mut send_conditional = force_revalidation;

			match payload.policy.before_request(&request, SystemTime::now()) {
				BeforeRequest::Fresh(_) if !force_revalidation => {
					return Ok(PreparedRequest::UseCached { jwks: payload.jwks.clone() });
				},
				BeforeRequest::Stale { request: parts, matches } if matches => {
					request = Request::from_parts(parts, ());
					send_conditional = true;
				},
				_ => {},
			}

			if send_conditional
				&& let Some(etag) = &payload.etag
				&& let Ok(value) = HeaderValue::from_str(etag)
			{
				request.headers_mut().insert(IF_NONE_MATCH, value);
			}
		}

		Ok(PreparedRequest::Send(Box::new(request)))
	}

	async fn perform_fetch_with_retry(
		&self,
		request: Request<()>,
		existing: Option<CachePayload>,
		mode: FetchMode,
		force_revalidation: bool,
	) -> Result<RefreshOutcome> {
		let mut executor = RetryExecutor::new(&self.registration.retry_policy);
		let mut last_error: Option<Error> = None;
		let mut last_backoff: Option<Duration> = None;
		let request = request;

		while let AttemptBudget::Granted { timeout } = executor.attempt_budget() {
			let attempt_started = Instant::now();
			let fetch = fetch_jwks(&self.client, &self.registration, &request, timeout).await;

			match fetch {
				Ok(fetch) => {
					let now = Instant::now();
					let payload = match (&fetch.jwks, existing.as_ref()) {
						(Some(fresh_jwks), _) => {
							let freshness =
								evaluate_freshness(&self.registration, &fetch.exchange)?;

							self.build_payload(
								fresh_jwks.clone(),
								freshness,
								fetch.etag.clone(),
								fetch.last_modified,
								now,
								Utc::now(),
							)
						},
						(None, Some(previous)) => {
							let revalidation = evaluate_revalidation(
								&self.registration,
								&previous.policy,
								&fetch.exchange.request,
								&fetch.exchange.response,
							)?;
							let updated_etag = extract_header(&revalidation.response, &ETAG)
								.or_else(|| previous.etag.clone());

							self.build_payload(
								previous.jwks.clone(),
								revalidation.freshness,
								updated_etag,
								extract_last_modified(&revalidation.response)
									.or(previous.last_modified),
								now,
								Utc::now(),
							)
						},
						(None, None) => {
							return Err(Error::Cache(
								"Received 304 status without a cached payload.".into(),
							));
						},
					};

					let jwks = payload.jwks.clone();

					self.commit_success(mode, payload).await;
					self.observe_refresh_success(attempt_started.elapsed());

					return Ok(RefreshOutcome::Updated { jwks, from_cache: false });
				},
				Err(err) => {
					last_error = Some(err);

					if !executor.can_retry() {
						break;
					}

					if let Some(delay) = executor.next_backoff() {
						last_backoff = Some(delay);

						if !delay.is_zero() {
							time::sleep(delay).await;
						}
						continue;
					}

					break;
				},
			}
		}

		let now = Instant::now();

		match mode {
			FetchMode::Initial => {
				let mut entry = self.entry.write().await;

				entry.invalidate();
			},
			FetchMode::Refresh => {
				let mut entry = self.entry.write().await;

				entry.refresh_failure(now, last_backoff);
			},
		}

		self.observe_refresh_error();

		if !force_revalidation
			&& let Some(payload) = existing
			&& payload.can_serve_stale(now)
		{
			return Ok(RefreshOutcome::Stale(payload.jwks));
		}

		Err(last_error.unwrap_or_else(|| Error::Cache("Refresh attempts exhausted.".into())))
	}

	async fn commit_success(&self, mode: FetchMode, payload: CachePayload) {
		let mut entry = self.entry.write().await;

		match mode {
			FetchMode::Initial => entry.load_success(payload),
			FetchMode::Refresh => entry.refresh_success(payload),
		}
	}

	fn build_payload(
		&self,
		jwks: Arc<JwkSet>,
		freshness: Freshness,
		etag: Option<String>,
		last_modified: Option<DateTime<Utc>>,
		now: Instant,
		refreshed_at: DateTime<Utc>,
	) -> CachePayload {
		let ttl = freshness.ttl;
		let expires_at = now + ttl;
		let mut refresh_at = if self.registration.refresh_early >= ttl {
			now
		} else {
			expires_at - self.registration.refresh_early
		};

		if !self.registration.prefetch_jitter.is_zero() {
			let jitter = random_jitter(self.registration.prefetch_jitter);

			if refresh_at > now + jitter {
				refresh_at -= jitter;
			}
		}

		let stale_deadline = if self.registration.stale_while_error.is_zero() {
			None
		} else {
			Some(expires_at + self.registration.stale_while_error)
		};

		CachePayload {
			jwks,
			policy: freshness.policy,
			etag,
			last_modified,
			last_refresh_at: refreshed_at,
			expires_at,
			next_refresh_at: refresh_at,
			stale_deadline,
			retry_backoff: None,
			error_count: 0,
		}
	}

	fn observe_hit(&self, stale: bool) {
		let tenant = &self.registration.tenant_id;
		let provider = &self.registration.provider_id;

		metrics::record_resolve_hit(tenant, provider, stale);

		self.metrics.record_hit(stale);
	}

	fn observe_miss(&self) {
		let tenant = &self.registration.tenant_id;
		let provider = &self.registration.provider_id;

		metrics::record_resolve_miss(tenant, provider);

		self.metrics.record_miss();
	}

	fn observe_refresh_success(&self, duration: Duration) {
		let tenant = &self.registration.tenant_id;
		let provider = &self.registration.provider_id;

		metrics::record_refresh_success(tenant, provider, duration);

		self.metrics.record_refresh_success(duration);
	}

	fn observe_refresh_error(&self) {
		let tenant = &self.registration.tenant_id;
		let provider = &self.registration.provider_id;

		metrics::record_refresh_error(tenant, provider);

		self.metrics.record_refresh_error();
	}
}

/// Snapshot of cache state captured for status reporting.
#[derive(Clone, Debug)]
pub struct CacheSnapshot {
	/// Monotonic instant when the snapshot was taken.
	pub captured_at: Instant,
	/// Wall-clock timestamp that aligns with `captured_at`.
	pub captured_at_wallclock: DateTime<Utc>,
	/// Cache state recorded at capture time.
	pub state: CacheState,
}
impl CacheSnapshot {
	/// Convert a monotonic instant drawn from the cached payload into UTC.
	pub fn to_datetime(&self, instant: Instant) -> Option<DateTime<Utc>> {
		if let Some(delta) = instant.checked_duration_since(self.captured_at) {
			let chrono = TimeDelta::from_std(delta).ok()?;

			self.captured_at_wallclock.checked_add_signed(chrono)
		} else if let Some(delta) = self.captured_at.checked_duration_since(instant) {
			let chrono = TimeDelta::from_std(delta).ok()?;

			self.captured_at_wallclock.checked_sub_signed(chrono)
		} else {
			None
		}
	}
}

#[derive(Clone, Copy, Debug)]
enum FetchMode {
	Initial,
	Refresh,
}

#[derive(Debug)]
enum RefreshOutcome {
	Updated { jwks: Arc<JwkSet>, from_cache: bool },
	Stale(Arc<JwkSet>),
}

#[derive(Clone, Copy, Debug)]
enum RefreshTrigger {
	Background,
	Blocking,
	None,
}

#[derive(Debug)]
enum PreparedRequest {
	UseCached { jwks: Arc<JwkSet> },
	Send(Box<Request<()>>),
}

fn random_jitter(max: Duration) -> Duration {
	if max.is_zero() {
		return Duration::ZERO;
	}

	let mut rng = rand::rng();
	let jitter = rng.random_range(0.0..=max.as_secs_f64());

	Duration::from_secs_f64(jitter)
}

fn extract_header(response: &Response<()>, name: &HeaderName) -> Option<String> {
	response.headers().get(name).and_then(|value| value.to_str().ok()).map(|s| s.to_string())
}

fn extract_last_modified(response: &Response<()>) -> Option<DateTime<Utc>> {
	response
		.headers()
		.get(LAST_MODIFIED)
		.and_then(|value| value.to_str().ok())
		.and_then(|raw| httpdate::parse_http_date(raw).ok())
		.map(<DateTime<Utc>>::from)
}
