//! High-performance async JWKS cache with ETag revalidation, early refresh, and multi-tenant
//! support — built for modern Rust identity systems.

#![deny(clippy::all, missing_docs, unused_crate_dependencies)]

pub mod cache;
pub mod http;
#[cfg(feature = "metrics")] pub mod metrics;
pub mod security;

mod error;
mod registry;
mod _prelude {
	pub use std::{
		sync::Arc,
		time::{Duration, SystemTime},
	};

	pub use chrono::{DateTime, TimeDelta, Utc};
	pub use tokio::time::Instant;

	pub use crate::{Error, Result};
}
#[cfg(feature = "prometheus")] pub use crate::metrics::install_default_exporter;
#[cfg(feature = "metrics")] pub use crate::registry::StatusMetric;
pub use crate::{
	error::{Error, Result},
	registry::{
		IdentityProviderRegistration, JitterStrategy, PersistentSnapshot, ProviderState,
		ProviderStatus, Registry, RegistryBuilder, RetryPolicy,
	},
};

#[cfg(test)]
mod _test {
	use metrics_util as _;
	use tracing_subscriber as _;
	use wiremock as _;
}
