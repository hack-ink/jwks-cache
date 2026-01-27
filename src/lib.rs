//! High-performance async JWKS cache with ETag revalidation, early refresh, and multi-tenant
//! support — built for modern Rust identity systems.

#![deny(clippy::all, missing_docs, unused_crate_dependencies)]

pub mod cache;
pub mod http;
pub mod metrics;
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
#[cfg(test)]
mod _test {
	use tracing_subscriber as _;
	use wiremock as _;
}

pub use crate::{
	error::{Error, Result},
	metrics::install_default_exporter,
	registry::{
		IdentityProviderRegistration, JitterStrategy, PersistentSnapshot, ProviderState,
		ProviderStatus, Registry, RegistryBuilder, RetryPolicy, StatusMetric,
	},
};
