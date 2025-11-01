//! Crate-wide error types and `Result` alias.

/// Library-wide result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Unified error type for the JWKS cache crate.
#[allow(missing_docs)]
#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error(transparent)]
	Io(#[from] std::io::Error),
	#[error(transparent)]
	SystemTime(#[from] std::time::SystemTimeError),

	#[error(transparent)]
	Http(#[from] http::Error),
	#[error(transparent)]
	Jsonwebtoken(#[from] jsonwebtoken::errors::Error),
	#[error(transparent)]
	Reqwest(#[from] reqwest::Error),
	#[error(transparent)]
	Serde(#[from] serde_json::Error),
	#[error(transparent)]
	Url(#[from] url::ParseError),

	#[cfg(feature = "redis")]
	#[error(transparent)]
	Redis(#[from] redis::RedisError),

	#[error("Cache error: {0}")]
	Cache(String),
	#[error("Upstream HTTP status {status} from {url}: {body:?}")]
	HttpStatus { status: http::StatusCode, url: url::Url, body: Option<String> },
	#[error("Metrics error: {0}")]
	Metrics(String),
	#[error("Provider not registered for tenant '{tenant}' and id '{provider}'.")]
	NotRegistered { tenant: String, provider: String },
	#[error("Security violation: {0}")]
	Security(String),
	#[error("Validation failed for {field}: {reason}")]
	Validation { field: &'static str, reason: String },
}
impl<T> From<metrics::SetRecorderError<T>> for Error
where
	T: std::fmt::Display,
{
	fn from(value: metrics::SetRecorderError<T>) -> Self {
		Self::Metrics(value.to_string())
	}
}
