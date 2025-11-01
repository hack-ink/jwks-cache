//! Tenant/provider registry and configuration validation.
//!
//! The registry owns tenant registrations, cache metadata, and optional persistence wiring.

// std
use std::{cell::RefCell, collections::HashMap, mem};
// crates.io
use jsonwebtoken::jwk::JwkSet;
use rand::{Rng, SeedableRng, rngs::SmallRng};
#[cfg(feature = "redis")] use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use url::Url;
// self
use crate::{
	_prelude::*,
	cache::{
		manager::{CacheManager, CacheSnapshot},
		state::CacheState,
	},
	metrics::{ProviderMetrics, ProviderMetricsSnapshot},
	security::{self, SpkiFingerprint},
};

thread_local! {
	static SMALL_RNG: RefCell<SmallRng> = RefCell::new(SmallRng::from_rng(&mut rand::rng()));
}

/// Default refresh lead time before TTL expiry.
pub const DEFAULT_REFRESH_EARLY: Duration = Duration::from_secs(30);
/// Default stale-while-error window.
pub const DEFAULT_STALE_WHILE_ERROR: Duration = Duration::from_secs(60);
/// Minimum accepted TTL for upstream responses.
pub const MIN_TTL_FLOOR: Duration = Duration::from_secs(30);
/// Default maximum TTL clamp.
pub const DEFAULT_MAX_TTL: Duration = Duration::from_secs(60 * 60 * 24);
/// Default size guard (1 MiB).
pub const DEFAULT_MAX_RESPONSE_BYTES: u64 = 1_048_576;
/// Default prefetch jitter.
pub const DEFAULT_PREFETCH_JITTER: Duration = Duration::from_secs(5);
/// Maximum redirect depth.
pub const MAX_REDIRECTS: u8 = 10;

/// Supported jitter strategies for retry policies.
#[derive(Clone, Debug, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JitterStrategy {
	/// No jitter; deterministic backoff schedule.
	None,
	/// Full jitter; randomize delay between 0 and current backoff.
	#[default]
	Full,
	/// Decorrelated jitter per AWS architecture guidance.
	Decorrelated,
}

/// Public representation of provider lifecycle state.
#[derive(Clone, Debug, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ProviderState {
	/// No JWKS payload has been cached yet.
	Empty,
	/// Initial fetch operation is currently running.
	Loading,
	/// Fresh JWKS payload is available for requests.
	Ready,
	/// Cache is serving while a refresh is in progress.
	Refreshing,
}

/// Retry configuration for HTTP fetch operations.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetryPolicy {
	/// Maximum number of retry attempts to perform after the initial request.
	pub max_retries: u32,
	/// Timeout applied to each individual HTTP attempt.
	pub attempt_timeout: Duration,
	/// Initial delay before retrying after a failure.
	pub initial_backoff: Duration,
	/// Upper bound applied to exponential backoff growth.
	pub max_backoff: Duration,
	/// Overall deadline that bounds the entire retry sequence.
	pub deadline: Duration,
	/// Strategy used to randomize the computed backoff.
	#[serde(default)]
	pub jitter: JitterStrategy,
}
impl RetryPolicy {
	/// Validate invariants for retry configuration.
	pub fn validate(&self) -> Result<()> {
		if self.attempt_timeout < Duration::from_millis(100) {
			return Err(Error::Validation {
				field: "retry_policy.attempt_timeout",
				reason: "Must be at least 100 ms.".into(),
			});
		}
		if self.initial_backoff.is_zero() {
			return Err(Error::Validation {
				field: "retry_policy.initial_backoff",
				reason: "Must be greater than zero.".into(),
			});
		}
		if self.max_backoff < self.initial_backoff {
			return Err(Error::Validation {
				field: "retry_policy.max_backoff",
				reason: "Must be greater than or equal to initial_backoff.".into(),
			});
		}
		if self.deadline < self.attempt_timeout {
			return Err(Error::Validation {
				field: "retry_policy.deadline",
				reason: "Must be greater than or equal to attempt_timeout.".into(),
			});
		}
		Ok(())
	}

	/// Compute backoff for a retry attempt using the selected jitter strategy.
	pub fn compute_backoff(&self, attempt: u32) -> Duration {
		self.default_backoff(attempt)
	}

	/// Default exponential backoff with jitter following the AWS architecture guidance.
	pub fn default_backoff(&self, attempt: u32) -> Duration {
		let exponent = attempt.min(32);
		let base = self.initial_backoff.mul_f64(2f64.powi(exponent as i32));
		let bounded = base.min(self.max_backoff).max(self.initial_backoff);

		self.apply_jitter(bounded, attempt)
	}

	fn apply_jitter(&self, bounded: Duration, attempt: u32) -> Duration {
		match self.jitter {
			JitterStrategy::None => bounded,
			JitterStrategy::Full => {
				let lower = bounded.mul_f64(0.8).max(self.initial_backoff);
				let upper = bounded.min(self.max_backoff);

				random_within(lower, upper)
			},
			JitterStrategy::Decorrelated => {
				let prev = if attempt == 0 { self.initial_backoff } else { bounded };
				let ceiling = self.max_backoff.min(prev.mul_f64(3.0));

				random_within(self.initial_backoff, ceiling.max(self.initial_backoff))
			},
		}
	}
}
impl Default for RetryPolicy {
	fn default() -> Self {
		Self {
			max_retries: 2,
			attempt_timeout: Duration::from_secs(3),
			initial_backoff: Duration::from_millis(250),
			max_backoff: Duration::from_secs(2),
			deadline: Duration::from_secs(8),
			jitter: JitterStrategy::Full,
		}
	}
}

/// Registration describing how to fetch and maintain JWKS for a provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityProviderRegistration {
	/// Tenant identifier used for metrics, caching, and persistence scope.
	pub tenant_id: String,
	/// Provider identifier unique within the tenant.
	pub provider_id: String,
	/// URL of the JWKS endpoint to fetch signing keys from.
	pub jwks_url: Url,
	/// Whether HTTPS is required for JWKS retrieval.
	#[serde(default = "default_true")]
	pub require_https: bool,
	/// Optional allowlist of domains permitted for redirects.
	#[serde(default, deserialize_with = "crate::security::deserialize_allowed_domains")]
	pub allowed_domains: Vec<String>,
	/// Lead time before expiry to trigger proactive refresh.
	#[serde(default = "default_refresh_early")]
	pub refresh_early: Duration,
	/// Duration to continue serving stale data when refresh fails.
	#[serde(default = "default_stale_while_error")]
	pub stale_while_error: Duration,
	/// Minimum TTL applied to upstream responses.
	#[serde(default = "default_min_ttl")]
	pub min_ttl: Duration,
	/// Maximum TTL applied to upstream responses.
	#[serde(default = "default_max_ttl")]
	pub max_ttl: Duration,
	/// Maximum size allowed for JWKS payloads in bytes.
	#[serde(default = "default_max_response_bytes")]
	pub max_response_bytes: u64,
	/// TTL applied when persisting negative cache outcomes.
	#[serde(default)]
	pub negative_cache_ttl: Duration,
	/// Maximum number of redirects to follow during fetch.
	#[serde(default = "default_max_redirects")]
	pub max_redirects: u8,
	/// Optional SPKI fingerprints used for TLS pinning.
	#[serde(default)]
	pub pinned_spki: Vec<SpkiFingerprint>,
	/// Random jitter applied when scheduling proactive refreshes.
	#[serde(default = "default_prefetch_jitter")]
	pub prefetch_jitter: Duration,
	/// Retry policy configuration for JWKS fetch attempts.
	#[serde(default)]
	pub retry_policy: RetryPolicy,
}
impl IdentityProviderRegistration {
	/// Construct a new registration with default cache settings.
	pub fn new(
		tenant_id: impl Into<String>,
		provider_id: impl Into<String>,
		jwks_url: impl AsRef<str>,
	) -> Result<Self> {
		let jwks_url = Url::parse(jwks_url.as_ref())?;

		Ok(Self {
			tenant_id: tenant_id.into(),
			provider_id: provider_id.into(),
			jwks_url,
			require_https: true,
			allowed_domains: Vec::new(),
			refresh_early: DEFAULT_REFRESH_EARLY,
			stale_while_error: DEFAULT_STALE_WHILE_ERROR,
			min_ttl: MIN_TTL_FLOOR,
			max_ttl: DEFAULT_MAX_TTL,
			max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
			negative_cache_ttl: Duration::ZERO,
			max_redirects: 3,
			pinned_spki: Vec::new(),
			prefetch_jitter: DEFAULT_PREFETCH_JITTER,
			retry_policy: RetryPolicy::default(),
		})
	}

	/// Canonicalise the domain allowlist in-place.
	pub fn normalize_allowed_domains(&mut self) {
		let domains = mem::take(&mut self.allowed_domains);

		self.allowed_domains = security::normalize_allowlist(domains);
	}

	/// Set HTTPS requirement to the desired value.
	pub fn with_require_https(mut self, require_https: bool) -> Self {
		self.require_https = require_https;

		self
	}

	/// Validate the registration against the documented constraints.
	pub fn validate(&self) -> Result<()> {
		validate_tenant_id(&self.tenant_id)?;
		validate_provider_id(&self.provider_id)?;

		if self.require_https {
			security::enforce_https(&self.jwks_url)?;
		}

		if let Some(host) = self.jwks_url.host_str() {
			if !security::host_is_allowed(host, &self.allowed_domains) {
				return Err(Error::Validation {
					field: "jwks_url",
					reason: "Host is not within the allowed_domains allowlist.".into(),
				});
			}
		} else {
			return Err(Error::Validation {
				field: "jwks_url",
				reason: "Must include a host component.".into(),
			});
		}

		if self.refresh_early < Duration::from_secs(1) {
			return Err(Error::Validation {
				field: "refresh_early",
				reason: "Must be at least 1 second.".into(),
			});
		}
		if self.min_ttl < MIN_TTL_FLOOR {
			return Err(Error::Validation {
				field: "min_ttl",
				reason: format!("Must be at least {:?}.", MIN_TTL_FLOOR),
			});
		}
		if self.max_ttl < self.min_ttl {
			return Err(Error::Validation {
				field: "max_ttl",
				reason: "Must be greater than or equal to min_ttl.".into(),
			});
		}
		if self.refresh_early >= self.max_ttl {
			return Err(Error::Validation {
				field: "refresh_early",
				reason: "Must be less than max_ttl.".into(),
			});
		}
		if self.max_response_bytes == 0 {
			return Err(Error::Validation {
				field: "max_response_bytes",
				reason: "Must be greater than zero.".into(),
			});
		}
		if self.max_redirects > MAX_REDIRECTS {
			return Err(Error::Validation {
				field: "max_redirects",
				reason: format!("Must be less than or equal to {}.", MAX_REDIRECTS),
			});
		}
		if !self.negative_cache_ttl.is_zero() && self.negative_cache_ttl < Duration::from_secs(1) {
			return Err(Error::Validation {
				field: "negative_cache_ttl",
				reason: "Must be zero or at least one second.".into(),
			});
		}

		self.retry_policy.validate()?;

		for domain in &self.allowed_domains {
			if let Some(canonical) = security::canonicalize_dns_name(domain) {
				if canonical != *domain {
					return Err(Error::Validation {
						field: "allowed_domains",
						reason: "Entries must be canonical hostnames (lowercase, no trailing dot)."
							.into(),
					});
				}
			} else {
				return Err(Error::Validation {
					field: "allowed_domains",
					reason: "Entries must be non-empty hostnames.".into(),
				});
			}
		}

		Ok(())
	}
}

/// Snapshot of cache payload persisted to external storage.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistentSnapshot {
	/// Tenant identifier associated with the snapshot.
	pub tenant_id: String,
	/// Provider identifier within the tenant scope.
	pub provider_id: String,
	/// Serialized JWKS payload captured from the cache.
	pub jwks_json: String,
	/// Entity tag returned by the JWKS endpoint, if present.
	pub etag: Option<String>,
	/// Last-Modified timestamp advertised by the JWKS endpoint.
	#[serde(default)]
	pub last_modified: Option<DateTime<Utc>>,
	/// UTC timestamp when the cached payload expires.
	pub expires_at: DateTime<Utc>,
	/// UTC timestamp when the snapshot was persisted.
	pub persisted_at: DateTime<Utc>,
}
impl PersistentSnapshot {
	/// Validate snapshot metadata aligns with registration expectations.
	pub fn validate(&self, registration: &IdentityProviderRegistration) -> Result<()> {
		if self.jwks_json.len() as u64 > registration.max_response_bytes {
			return Err(Error::Validation {
				field: "jwks_json",
				reason: format!(
					"Snapshot exceeds max_response_bytes ({} bytes).",
					registration.max_response_bytes
				),
			});
		}

		if self.tenant_id != registration.tenant_id {
			return Err(Error::Validation {
				field: "tenant_id",
				reason: "Snapshot tenant does not match registration.".into(),
			});
		}
		if self.provider_id != registration.provider_id {
			return Err(Error::Validation {
				field: "provider_id",
				reason: "Snapshot provider does not match registration.".into(),
			});
		}

		if let Some(etag) = &self.etag
			&& !etag.is_ascii()
		{
			return Err(Error::Validation { field: "etag", reason: "ETag must be ASCII.".into() });
		}

		if self.expires_at < self.persisted_at {
			return Err(Error::Validation {
				field: "expires_at",
				reason: "Cannot be earlier than persisted_at.".into(),
			});
		}

		Ok(())
	}
}

/// Internal key mapping tenants and providers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TenantProviderKey {
	pub tenant_id: String,
	pub provider_id: String,
}
impl TenantProviderKey {
	pub fn new(tenant_id: impl Into<String>, provider_id: impl Into<String>) -> Self {
		Self { tenant_id: tenant_id.into(), provider_id: provider_id.into() }
	}
}

/// Builder for [`Registry`] enabling multi-tenant configuration.
#[derive(Debug, Default)]
pub struct RegistryBuilder {
	config: RegistryConfig,
}
impl RegistryBuilder {
	/// Create a builder with default configuration.
	pub fn new() -> Self {
		Self::default()
	}

	/// Enforce HTTPS for registrations (enabled by default).
	pub fn require_https(mut self, require_https: bool) -> Self {
		self.config.require_https = require_https;

		self
	}

	/// Override the default refresh-early offset applied to registrations.
	pub fn default_refresh_early(mut self, value: Duration) -> Self {
		self.config.default_refresh_early = value;

		self
	}

	/// Override the default stale-while-error window applied to registrations.
	pub fn default_stale_while_error(mut self, value: Duration) -> Self {
		self.config.default_stale_while_error = value;

		self
	}

	/// Add an entry to the global domain allowlist.
	pub fn add_allowed_domain(mut self, domain: impl Into<String>) -> Self {
		let raw = domain.into();

		if let Some(domain) = security::canonicalize_dns_name(&raw)
			&& !self.config.allowed_domains.contains(&domain)
		{
			self.config.allowed_domains.push(domain);
		}

		self
	}

	/// Replace the global domain allowlist.
	pub fn allowed_domains<I, S>(mut self, domains: I) -> Self
	where
		I: IntoIterator<Item = S>,
		S: Into<String>,
	{
		self.config.allowed_domains.clear();

		for domain in domains {
			self = self.add_allowed_domain(domain);
		}

		self
	}

	#[cfg(feature = "redis")]
	/// Configure Redis-backed persistence for snapshots.
	pub fn with_redis_client(mut self, client: redis::Client) -> Self {
		self.config.persistence = Some(RedisPersistence::new(client));

		self
	}

	#[cfg(feature = "redis")]
	/// Adjust the Redis key namespace (defaults to `jwks-cache`).
	pub fn redis_namespace(mut self, namespace: impl Into<String>) -> Self {
		if let Some(persistence) = self.config.persistence.as_mut() {
			persistence.namespace = Arc::from(namespace.into());
		} else {
			panic!("Redis client must be configured before setting namespace.");
		}

		self
	}

	/// Finalise the configuration and construct a [`Registry`].
	pub fn build(self) -> Registry {
		let mut config = self.config;

		config.allowed_domains = security::normalize_allowlist(config.allowed_domains);

		Registry {
			inner: Arc::new(RwLock::new(RegistryState { providers: HashMap::new() })),
			config: Arc::new(config),
		}
	}
}

/// Registry state container.
#[derive(Clone, Debug)]
pub struct Registry {
	inner: Arc<RwLock<RegistryState>>,
	config: Arc<RegistryConfig>,
}
impl Registry {
	/// Create a new registry instance with defaults.
	pub fn new() -> Self {
		Self::builder().build()
	}

	/// Create a [`RegistryBuilder`] for advanced configuration.
	pub fn builder() -> RegistryBuilder {
		RegistryBuilder::new()
	}

	/// Register or update a provider configuration.
	pub async fn register(&self, mut registration: IdentityProviderRegistration) -> Result<()> {
		if self.config.require_https {
			if !registration.require_https {
				return Err(Error::Security(
					"Registry requires HTTPS for all provider registrations.".into(),
				));
			}
		} else {
			registration.require_https = false;
		}

		registration.normalize_allowed_domains();

		if registration.refresh_early == DEFAULT_REFRESH_EARLY {
			registration.refresh_early = self.config.default_refresh_early;
		}
		if registration.stale_while_error == DEFAULT_STALE_WHILE_ERROR {
			registration.stale_while_error = self.config.default_stale_while_error;
		}
		if registration.allowed_domains.is_empty() && !self.config.allowed_domains.is_empty() {
			registration.allowed_domains = self.config.allowed_domains.clone();
		}

		if let Some(host) = registration.jwks_url.host_str()
			&& !security::host_is_allowed(host, &self.config.allowed_domains)
		{
			return Err(Error::Security(format!(
				"Host '{host}' is not in the registry allowlist."
			)));
		}

		let key = TenantProviderKey::new(&registration.tenant_id, &registration.provider_id);
		let manager = CacheManager::new(registration.clone())?;
		let metrics = manager.metrics();
		let handle =
			Arc::new(ProviderHandle { registration: Arc::new(registration), manager, metrics });

		{
			let mut state = self.inner.write().await;

			state.providers.insert(key.clone(), handle.clone());
		}

		#[cfg(feature = "redis")]
		if let Some(persistence) = &self.config.persistence {
			if let Some(snapshot) = persistence.load(&key.tenant_id, &key.provider_id).await? {
				handle.manager.restore_snapshot(snapshot).await?;
			}
		}

		Ok(())
	}

	/// Resolve JWKS for a tenant/provider pair.
	pub async fn resolve(
		&self,
		tenant_id: &str,
		provider_id: &str,
		kid: Option<&str>,
	) -> Result<Arc<JwkSet>> {
		let key = TenantProviderKey::new(tenant_id, provider_id);
		let handle = {
			let state = self.inner.read().await;

			state.providers.get(&key).cloned()
		};
		let handle = handle.ok_or_else(|| Error::NotRegistered {
			tenant: tenant_id.to_string(),
			provider: provider_id.to_string(),
		})?;

		handle.manager.resolve(kid).await
	}

	/// Trigger a manual refresh for a registered provider.
	pub async fn refresh(&self, tenant_id: &str, provider_id: &str) -> Result<()> {
		let key = TenantProviderKey::new(tenant_id, provider_id);
		let handle = {
			let state = self.inner.read().await;
			state.providers.get(&key).cloned()
		};
		let handle = handle.ok_or_else(|| Error::NotRegistered {
			tenant: tenant_id.to_string(),
			provider: provider_id.to_string(),
		})?;

		handle.manager.trigger_refresh().await
	}

	/// Remove a provider registration if present.
	pub async fn unregister(&self, tenant_id: &str, provider_id: &str) -> Result<bool> {
		let key = TenantProviderKey::new(tenant_id, provider_id);
		let mut state = self.inner.write().await;

		Ok(state.providers.remove(&key).is_some())
	}

	/// Fetch status information for a specific provider.
	pub async fn provider_status(
		&self,
		tenant_id: &str,
		provider_id: &str,
	) -> Result<ProviderStatus> {
		let key = TenantProviderKey::new(tenant_id, provider_id);
		let handle = {
			let state = self.inner.read().await;

			state.providers.get(&key).cloned()
		};
		let handle = handle.ok_or_else(|| Error::NotRegistered {
			tenant: tenant_id.to_string(),
			provider: provider_id.to_string(),
		})?;

		Ok(handle.status().await)
	}

	/// Fetch status for every registered provider.
	pub async fn all_statuses(&self) -> Vec<ProviderStatus> {
		let handles: Vec<Arc<ProviderHandle>> = {
			let state = self.inner.read().await;
			state.providers.values().cloned().collect()
		};
		let mut statuses = Vec::with_capacity(handles.len());

		for handle in handles {
			statuses.push(handle.status().await);
		}

		statuses
	}

	/// Persist snapshots for every provider when persistence is configured.
	pub async fn persist_all(&self) -> Result<()> {
		#[cfg(feature = "redis")]
		{
			if let Some(persistence) = &self.config.persistence {
				let handles: Vec<Arc<ProviderHandle>> = {
					let state = self.inner.read().await;

					state.providers.values().cloned().collect()
				};
				let mut snapshots = Vec::new();

				for handle in handles {
					if let Some(snapshot) = handle.manager.persistent_snapshot().await? {
						snapshots.push(snapshot);
					}
				}

				persistence.persist(&snapshots).await?;
			}
		}

		Ok(())
	}

	/// Restore cached entries from persistence for all active registrations.
	pub async fn restore_from_persistence(&self) -> Result<()> {
		#[cfg(feature = "redis")]
		{
			if let Some(persistence) = &self.config.persistence {
				let handles: Vec<Arc<ProviderHandle>> = {
					let state = self.inner.read().await;

					state.providers.values().cloned().collect()
				};

				for handle in handles {
					if let Some(snapshot) = persistence
						.load(&handle.registration.tenant_id, &handle.registration.provider_id)
						.await?
					{
						handle.manager.restore_snapshot(snapshot).await?;
					}
				}
			}
		}

		Ok(())
	}
}
impl Default for Registry {
	fn default() -> Self {
		Self::new()
	}
}

/// Status projection for a provider, aligned with the OpenAPI contract.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderStatus {
	/// Tenant identifier that owns the provider.
	pub tenant_id: String,
	/// Provider identifier unique within the tenant.
	pub provider_id: String,
	/// Lifecycle state currently reported for the provider.
	pub state: ProviderState,
	/// Timestamp of the most recent successful refresh.
	pub last_refresh: Option<DateTime<Utc>>,
	/// Scheduled timestamp for the next refresh attempt.
	pub next_refresh: Option<DateTime<Utc>>,
	/// Expiration timestamp for the active payload, if available.
	pub expires_at: Option<DateTime<Utc>>,
	/// Consecutive error count observed during refresh attempts.
	pub error_count: u32,
	/// Ratio of cache hits to total requests.
	pub hit_rate: f64,
	/// Ratio of served responses that were stale.
	pub stale_serve_ratio: f64,
	/// Metrics emitted to describe provider performance.
	pub metrics: Vec<StatusMetric>,
}
impl ProviderStatus {
	fn from_components(
		registration: &IdentityProviderRegistration,
		snapshot: CacheSnapshot,
		metrics: ProviderMetricsSnapshot,
	) -> Self {
		let mut last_refresh = None;
		let mut next_refresh = None;
		let mut expires_at = None;
		let mut error_count = 0;
		let state = match &snapshot.state {
			CacheState::Empty => ProviderState::Empty,
			CacheState::Loading => ProviderState::Loading,
			CacheState::Ready(payload) => {
				last_refresh = Some(payload.last_refresh_at);
				next_refresh = snapshot.to_datetime(payload.next_refresh_at);
				expires_at = snapshot.to_datetime(payload.expires_at);
				error_count = payload.error_count;
				ProviderState::Ready
			},
			CacheState::Refreshing(payload) => {
				last_refresh = Some(payload.last_refresh_at);
				next_refresh = snapshot.to_datetime(payload.next_refresh_at);
				expires_at = snapshot.to_datetime(payload.expires_at);
				error_count = payload.error_count;
				ProviderState::Refreshing
			},
		};
		let tenant = &registration.tenant_id;
		let provider = &registration.provider_id;
		let mut status_metrics = vec![
			StatusMetric::new(
				"jwks_cache_requests_total",
				metrics.total_requests as f64,
				tenant,
				provider,
			),
			StatusMetric::new("jwks_cache_hits_total", metrics.cache_hits as f64, tenant, provider),
			StatusMetric::new(
				"jwks_cache_stale_total",
				metrics.stale_serves as f64,
				tenant,
				provider,
			),
			StatusMetric::new(
				"jwks_cache_refresh_errors_total",
				metrics.refresh_errors as f64,
				tenant,
				provider,
			),
		];

		if let Some(last_micros) = metrics.last_refresh_micros {
			status_metrics.push(StatusMetric::new(
				"jwks_cache_last_refresh_micros",
				last_micros as f64,
				tenant,
				provider,
			));
		}

		Self {
			tenant_id: tenant.clone(),
			provider_id: provider.clone(),
			state,
			last_refresh,
			next_refresh,
			expires_at,
			error_count,
			hit_rate: metrics.hit_rate(),
			stale_serve_ratio: metrics.stale_ratio(),
			metrics: status_metrics,
		}
	}
}

/// Metric sample used in provider status responses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatusMetric {
	/// Metric name following the monitoring schema.
	pub name: String,
	/// Numeric value captured for the metric.
	pub value: f64,
	/// Additional labels enriching the metric sample.
	#[serde(default)]
	pub labels: HashMap<String, String>,
}
impl StatusMetric {
	fn new(name: impl Into<String>, value: f64, tenant: &str, provider: &str) -> Self {
		let mut labels = HashMap::with_capacity(2);

		labels.insert("tenant".into(), tenant.into());
		labels.insert("provider".into(), provider.into());

		Self { name: name.into(), value, labels }
	}
}

#[derive(Debug)]
struct RegistryConfig {
	require_https: bool,
	default_refresh_early: Duration,
	default_stale_while_error: Duration,
	allowed_domains: Vec<String>,
	#[cfg(feature = "redis")]
	persistence: Option<RedisPersistence>,
}
impl Default for RegistryConfig {
	fn default() -> Self {
		Self {
			require_https: true,
			default_refresh_early: DEFAULT_REFRESH_EARLY,
			default_stale_while_error: DEFAULT_STALE_WHILE_ERROR,
			allowed_domains: Vec::new(),
			#[cfg(feature = "redis")]
			persistence: None,
		}
	}
}

#[derive(Debug)]
struct ProviderHandle {
	registration: Arc<IdentityProviderRegistration>,
	manager: CacheManager,
	metrics: Arc<ProviderMetrics>,
}
impl ProviderHandle {
	async fn status(&self) -> ProviderStatus {
		let snapshot = self.manager.snapshot().await;
		let metrics = self.metrics.snapshot();

		ProviderStatus::from_components(&self.registration, snapshot, metrics)
	}
}

#[derive(Debug)]
struct RegistryState {
	// TODO: Consider replacing the RwLock<HashMap> with DashMap if contention becomes measurable.
	providers: HashMap<TenantProviderKey, Arc<ProviderHandle>>,
}

#[cfg(feature = "redis")]
#[derive(Clone, Debug)]
struct RedisPersistence {
	client: redis::Client,
	namespace: Arc<str>,
}
#[cfg(feature = "redis")]
impl RedisPersistence {
	fn new(client: redis::Client) -> Self {
		Self { client, namespace: Arc::from("jwks-cache") }
	}

	async fn persist(&self, snapshots: &[PersistentSnapshot]) -> Result<()> {
		if snapshots.is_empty() {
			return Ok(());
		}

		let mut conn = self.client.get_multiplexed_async_connection().await?;

		for snapshot in snapshots {
			let key = self.key(&snapshot.tenant_id, &snapshot.provider_id);
			let payload = serde_json::to_string(snapshot)?;
			let ttl = (snapshot.expires_at - Utc::now())
				.to_std()
				.unwrap_or_else(|_| Duration::from_secs(1));
			let ttl_secs = ttl.as_secs().max(1);

			conn.set_ex::<_, _, ()>(key, payload, ttl_secs).await?;
		}

		Ok(())
	}

	async fn load(&self, tenant: &str, provider: &str) -> Result<Option<PersistentSnapshot>> {
		let mut conn = self.client.get_multiplexed_async_connection().await?;
		let key = self.key(tenant, provider);
		let value: Option<String> = conn.get(key).await?;

		if let Some(json) = value {
			let snapshot: PersistentSnapshot = serde_json::from_str(&json)?;

			Ok(Some(snapshot))
		} else {
			Ok(None)
		}
	}

	fn key(&self, tenant: &str, provider: &str) -> String {
		format!("{}:{tenant}:{provider}", self.namespace)
	}
}

fn random_within(min: Duration, max: Duration) -> Duration {
	if max <= min {
		return max;
	}
	SMALL_RNG.with(|cell| {
		let mut rng = cell.borrow_mut();
		let nanos = max.as_nanos() - min.as_nanos();
		let jitter = rng.random_range(0..=nanos.min(u64::MAX as u128));

		min + Duration::from_nanos(jitter as u64)
	})
}

fn default_true() -> bool {
	true
}

fn default_refresh_early() -> Duration {
	DEFAULT_REFRESH_EARLY
}

fn default_stale_while_error() -> Duration {
	DEFAULT_STALE_WHILE_ERROR
}

fn default_min_ttl() -> Duration {
	MIN_TTL_FLOOR
}

fn default_max_ttl() -> Duration {
	DEFAULT_MAX_TTL
}

fn default_max_response_bytes() -> u64 {
	DEFAULT_MAX_RESPONSE_BYTES
}

fn default_max_redirects() -> u8 {
	3
}

fn default_prefetch_jitter() -> Duration {
	DEFAULT_PREFETCH_JITTER
}

fn validate_tenant_id(value: &str) -> Result<()> {
	if value.is_empty() {
		return Err(Error::Validation { field: "tenant_id", reason: "Must not be empty.".into() });
	}
	if value.len() > 64 {
		return Err(Error::Validation {
			field: "tenant_id",
			reason: "Must be 64 characters or fewer.".into(),
		});
	}
	if !value.as_bytes().iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-') {
		return Err(Error::Validation {
			field: "tenant_id",
			reason: "May only contain ASCII letters, numbers, and '-'.".into(),
		});
	}

	Ok(())
}

fn validate_provider_id(value: &str) -> Result<()> {
	if value.is_empty() {
		return Err(Error::Validation {
			field: "provider_id",
			reason: "Must not be empty.".into(),
		});
	}
	if value.len() > 64 {
		return Err(Error::Validation {
			field: "provider_id",
			reason: "Must be 64 characters or fewer.".into(),
		});
	}
	if !value.as_bytes().iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
		return Err(Error::Validation {
			field: "provider_id",
			reason: "May only contain ASCII letters, numbers, '-', or '_'.".into(),
		});
	}

	Ok(())
}
