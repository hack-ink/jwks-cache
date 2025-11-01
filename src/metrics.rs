//! Metrics helpers and per-provider telemetry bookkeeping.

// std
use std::sync::{
	OnceLock,
	atomic::{AtomicU64, Ordering},
};
// crates.io
use metrics::Label;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use smallvec::SmallVec;
// self
use crate::_prelude::*;

type LabelSet = SmallVec<[Label; 4]>;

const METRIC_REQUESTS_TOTAL: &str = "jwks_cache_requests_total";
const METRIC_HITS_TOTAL: &str = "jwks_cache_hits_total";
const METRIC_STALE_TOTAL: &str = "jwks_cache_stale_total";
const METRIC_MISSES_TOTAL: &str = "jwks_cache_misses_total";
const METRIC_REFRESH_TOTAL: &str = "jwks_cache_refresh_total";
const METRIC_REFRESH_DURATION: &str = "jwks_cache_refresh_duration_seconds";
const METRIC_REFRESH_ERRORS: &str = "jwks_cache_refresh_errors_total";

/// Shared Prometheus handle installed by [`install_default_exporter`].
static PROMETHEUS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Thread-safe metrics accumulator for a single provider registration.
#[derive(Debug, Default)]
pub struct ProviderMetrics {
	total_requests: AtomicU64,
	cache_hits: AtomicU64,
	stale_serves: AtomicU64,
	refresh_successes: AtomicU64,
	refresh_errors: AtomicU64,
	last_refresh_micros: AtomicU64,
}
impl ProviderMetrics {
	/// Create a new metrics accumulator.
	pub fn new() -> Arc<Self> {
		Arc::new(Self::default())
	}

	/// Record a hit outcome.
	pub fn record_hit(&self, stale: bool) {
		self.total_requests.fetch_add(1, Ordering::Relaxed);
		self.cache_hits.fetch_add(1, Ordering::Relaxed);
		if stale {
			self.stale_serves.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Record a miss outcome.
	pub fn record_miss(&self) {
		self.total_requests.fetch_add(1, Ordering::Relaxed);
	}

	/// Record a successful refresh and latency.
	pub fn record_refresh_success(&self, duration: Duration) {
		self.refresh_successes.fetch_add(1, Ordering::Relaxed);
		self.last_refresh_micros.store(duration.as_micros() as u64, Ordering::Relaxed);
	}

	/// Record refresh failure.
	pub fn record_refresh_error(&self) {
		self.refresh_errors.fetch_add(1, Ordering::Relaxed);
	}

	/// Take a point-in-time snapshot for status reporting.
	pub fn snapshot(&self) -> ProviderMetricsSnapshot {
		ProviderMetricsSnapshot {
			total_requests: self.total_requests.load(Ordering::Relaxed),
			cache_hits: self.cache_hits.load(Ordering::Relaxed),
			stale_serves: self.stale_serves.load(Ordering::Relaxed),
			refresh_successes: self.refresh_successes.load(Ordering::Relaxed),
			refresh_errors: self.refresh_errors.load(Ordering::Relaxed),
			last_refresh_micros: match self.last_refresh_micros.load(Ordering::Relaxed) {
				0 => None,
				value => Some(value),
			},
		}
	}
}

/// Read-only snapshot of per-provider telemetry counters.
#[derive(Clone, Debug)]
pub struct ProviderMetricsSnapshot {
	/// Total number of cache lookups observed.
	pub total_requests: u64,
	/// Count of lookups served from the cache.
	pub cache_hits: u64,
	/// Count of lookups served from stale payloads.
	pub stale_serves: u64,
	/// Count of successful refresh operations.
	pub refresh_successes: u64,
	/// Count of refresh attempts that resulted in errors.
	pub refresh_errors: u64,
	/// Microsecond latency of the most recent refresh.
	pub last_refresh_micros: Option<u64>,
}
impl ProviderMetricsSnapshot {
	/// Convenience method to compute the cache hit rate.
	pub fn hit_rate(&self) -> f64 {
		if self.total_requests == 0 {
			0.0
		} else {
			self.cache_hits as f64 / self.total_requests as f64
		}
	}

	/// Ratio of stale serves over total requests.
	pub fn stale_ratio(&self) -> f64 {
		if self.total_requests == 0 {
			0.0
		} else {
			self.stale_serves as f64 / self.total_requests as f64
		}
	}
}

/// Install the default Prometheus recorder backed by `metrics`.
///
/// Multiple invocations are safe; subsequent calls become no-ops once the recorder is installed.
pub fn install_default_exporter() -> Result<()> {
	if PROMETHEUS_HANDLE.get().is_some() {
		return Ok(());
	}

	let handle = PrometheusBuilder::new()
		.install_recorder()
		.map_err(|err| Error::Metrics(err.to_string()))?;
	let _ = PROMETHEUS_HANDLE.set(handle);

	Ok(())
}

/// Access the global Prometheus exporter handle when installed.
pub fn prometheus_handle() -> Option<&'static PrometheusHandle> {
	PROMETHEUS_HANDLE.get()
}

/// Record a cache hit, tagging whether it was served stale.
pub fn record_resolve_hit(tenant: &str, provider: &str, stale: bool) {
	let labels = base_labels(tenant, provider);

	metrics::counter!(METRIC_REQUESTS_TOTAL, labels.iter()).increment(1);
	metrics::counter!(METRIC_HITS_TOTAL, labels.iter()).increment(1);

	if stale {
		metrics::counter!(METRIC_STALE_TOTAL, labels.iter()).increment(1);
	}
}

/// Record a cache miss that required an upstream fetch.
pub fn record_resolve_miss(tenant: &str, provider: &str) {
	let labels = base_labels(tenant, provider);

	metrics::counter!(METRIC_REQUESTS_TOTAL, labels.iter()).increment(1);
	metrics::counter!(METRIC_MISSES_TOTAL, labels.iter()).increment(1);
}

/// Record a successful refresh attempt along with its latency.
pub fn record_refresh_success(tenant: &str, provider: &str, duration: Duration) {
	metrics::counter!(METRIC_REFRESH_TOTAL, status_labels(tenant, provider, "success").iter())
		.increment(1);
	metrics::histogram!(METRIC_REFRESH_DURATION, base_labels(tenant, provider).iter())
		.record(duration.as_secs_f64());
}

/// Record a failed refresh attempt.
pub fn record_refresh_error(tenant: &str, provider: &str) {
	metrics::counter!(METRIC_REFRESH_TOTAL, status_labels(tenant, provider, "error").iter())
		.increment(1);
	metrics::counter!(METRIC_REFRESH_ERRORS, base_labels(tenant, provider).iter()).increment(1);
}

fn base_labels(tenant: &str, provider: &str) -> LabelSet {
	let mut labels = LabelSet::with_capacity(2);

	labels.push(Label::new("tenant", tenant.to_owned()));
	labels.push(Label::new("provider", provider.to_owned()));

	labels
}

fn status_labels(tenant: &str, provider: &str, status: &'static str) -> LabelSet {
	let mut labels = base_labels(tenant, provider);

	labels.push(Label::new("status", status));

	labels
}

#[cfg(test)]
mod tests {
	// std
	use std::borrow::Borrow;
	// crates.io
	use metrics_util::{
		CompositeKey, MetricKind,
		debugging::{DebugValue, DebuggingRecorder},
	};
	// self
	use super::*;

	fn capture_metrics<F>(f: F) -> Vec<(CompositeKey, DebugValue)>
	where
		F: FnOnce(),
	{
		let recorder = DebuggingRecorder::new();
		let snapshotter = recorder.snapshotter();

		metrics::with_local_recorder(&recorder, f);

		snapshotter
			.snapshot()
			.into_vec()
			.into_iter()
			.map(|(key, _, _, value)| (key, value))
			.collect()
	}

	fn counter_value(
		snapshot: &[(CompositeKey, DebugValue)],
		name: &str,
		labels: &[(&str, &str)],
	) -> u64 {
		snapshot
			.iter()
			.find_map(|(key, value)| {
				(key.kind() == MetricKind::Counter
					&& Borrow::<str>::borrow(key.key().name()) == name
					&& labels_match(key, labels))
				.then(|| match value {
					DebugValue::Counter(value) => *value,
					_ => 0,
				})
			})
			.unwrap_or(0)
	}

	fn last_histogram_value(
		snapshot: &[(CompositeKey, DebugValue)],
		name: &str,
		labels: &[(&str, &str)],
	) -> Option<f64> {
		snapshot.iter().find_map(|(key, value)| {
			if key.kind() == MetricKind::Histogram
				&& Borrow::<str>::borrow(key.key().name()) == name
				&& labels_match(key, labels)
			{
				if let DebugValue::Histogram(values) = value {
					values.last().map(|v| v.into_inner())
				} else {
					None
				}
			} else {
				None
			}
		})
	}

	fn labels_match(key: &CompositeKey, expected: &[(&str, &str)]) -> bool {
		let mut labels: Vec<_> =
			key.key().labels().map(|label| (label.key(), label.value())).collect();

		labels.sort_unstable();

		let mut expected_sorted: Vec<_> = expected.to_vec();

		expected_sorted.sort_unstable();

		labels.len() == expected_sorted.len()
			&& labels
				.into_iter()
				.zip(expected_sorted.into_iter())
				.all(|((lk, lv), (ek, ev))| lk == ek && lv == ev)
	}

	#[test]
	fn records_hits_misses_and_stale_counts() {
		let snapshot = capture_metrics(|| {
			record_resolve_hit("tenant-a", "provider-1", false);
			record_resolve_hit("tenant-a", "provider-1", true);
			record_resolve_miss("tenant-a", "provider-1");
		});
		let base = [("tenant", "tenant-a"), ("provider", "provider-1")];

		assert_eq!(counter_value(&snapshot, "jwks_cache_requests_total", &base), 3);
		assert_eq!(counter_value(&snapshot, "jwks_cache_hits_total", &base), 2);
		assert_eq!(counter_value(&snapshot, "jwks_cache_misses_total", &base), 1);
		assert_eq!(counter_value(&snapshot, "jwks_cache_stale_total", &base), 1);
	}

	#[test]
	#[cfg_attr(miri, ignore)]
	fn records_refresh_success_and_errors() {
		let snapshot = capture_metrics(|| {
			record_refresh_success("tenant-b", "provider-2", std::time::Duration::from_millis(20));
			record_refresh_error("tenant-b", "provider-2");
		});
		let base = [("tenant", "tenant-b"), ("provider", "provider-2")];
		let success = [("tenant", "tenant-b"), ("provider", "provider-2"), ("status", "success")];
		let error = [("tenant", "tenant-b"), ("provider", "provider-2"), ("status", "error")];

		assert_eq!(counter_value(&snapshot, "jwks_cache_refresh_total", &success), 1);
		assert_eq!(counter_value(&snapshot, "jwks_cache_refresh_total", &error), 1);
		assert_eq!(counter_value(&snapshot, "jwks_cache_refresh_errors_total", &base), 1);

		let duration =
			last_histogram_value(&snapshot, "jwks_cache_refresh_duration_seconds", &base)
				.expect("refresh duration recorded");

		assert!((duration - 0.020).abs() < 1e-6, "expected ~20ms histogram, got {duration}");
	}
}
