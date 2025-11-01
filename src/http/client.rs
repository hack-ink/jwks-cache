//! HTTP client integration for JWKS retrieval.

// std
use std::marker::PhantomData;
// crates.io
use http::{
	HeaderMap, Request, Response, StatusCode,
	header::{CACHE_CONTROL, ETAG, LAST_MODIFIED},
};
use jsonwebtoken::jwk::JwkSet;
use reqwest::Client;
// self
use crate::{_prelude::*, registry::IdentityProviderRegistration, security};

/// HTTP exchange metadata captured for cache semantics evaluation.
#[derive(Clone, Debug)]
pub struct HttpExchange {
	/// HTTP request issued to the upstream JWKS endpoint.
	pub request: Request<()>,
	/// Response metadata returned from the upstream.
	pub response: Response<()>,
	/// Round-trip duration of the exchange.
	pub elapsed: Duration,
	/// Marker to signal that the response body is empty.
	_body: PhantomData<()>,
}
impl HttpExchange {
	/// Construct a new header-only exchange instance.
	pub fn new(request: Request<()>, response: Response<()>, elapsed: Duration) -> Self {
		Self { request, response, elapsed, _body: PhantomData }
	}

	/// Response headers helper.
	pub fn headers(&self) -> &HeaderMap {
		self.response.headers()
	}

	/// Response status helper.
	pub fn status(&self) -> StatusCode {
		self.response.status()
	}
}

/// Metadata returned from a JWKS HTTP fetch (200 or 304).
#[derive(Clone, Debug)]
pub struct HttpFetch {
	/// Captured HTTP exchange for diagnostics and cache evaluation.
	pub exchange: HttpExchange,
	/// Parsed JWKS payload when the origin returned content.
	pub jwks: Option<Arc<JwkSet>>,
	/// Entity tag validator advertised by the origin.
	pub etag: Option<String>,
	/// Last-Modified timestamp advertised by the origin.
	pub last_modified: Option<DateTime<Utc>>,
}

/// Execute an HTTP request to retrieve JWKS for the given registration.
pub async fn fetch_jwks(
	client: &Client,
	registration: &IdentityProviderRegistration,
	request: &Request<()>,
	attempt_timeout: Duration,
) -> Result<HttpFetch> {
	if registration.require_https {
		security::enforce_https(&registration.jwks_url)?;
	}

	let method = request.method().clone();
	let mut builder = client.request(method, registration.jwks_url.clone());

	for (name, value) in request.headers().iter() {
		builder = builder.header(name, value);
	}

	builder = builder.timeout(attempt_timeout);

	let start = Instant::now();
	let response = builder.send().await?;
	let elapsed = start.elapsed();
	let status = response.status();
	let headers = response.headers().clone();
	let mut response_builder = Response::builder().status(status);

	if let Some(existing) = response_builder.headers_mut() {
		existing.extend(headers.iter().map(|(name, value)| (name.clone(), value.clone())));
	}

	let response_template = response_builder.body(()).map_err(Error::from)?;
	let etag = response_template
		.headers()
		.get(ETAG)
		.and_then(|value| value.to_str().ok())
		.map(|s| s.to_string());
	let last_modified = response_template
		.headers()
		.get(LAST_MODIFIED)
		.and_then(|value| value.to_str().ok())
		.and_then(|raw| httpdate::parse_http_date(raw).ok())
		.map(DateTime::<Utc>::from);

	if status == StatusCode::NOT_MODIFIED {
		let exchange = HttpExchange::new(request.clone(), response_template, elapsed);

		return Ok(HttpFetch { exchange, jwks: None, etag, last_modified });
	}
	if !status.is_success() {
		let body = response.text().await.ok();

		return Err(Error::HttpStatus { status, url: registration.jwks_url.clone(), body });
	}

	let bytes = response.bytes().await?;

	if bytes.len() as u64 > registration.max_response_bytes {
		return Err(Error::Validation {
			field: "max_response_bytes",
			reason: format!(
				"Response size {size} bytes exceeds the configured guard of {limit} bytes.",
				size = bytes.len(),
				limit = registration.max_response_bytes
			),
		});
	}

	let jwks: JwkSet = serde_json::from_slice(&bytes)?;
	let exchange = HttpExchange::new(request.clone(), response_template, elapsed);

	tracing::debug!(
		tenant = %registration.tenant_id,
		provider = %registration.provider_id,
		status = %status,
		elapsed = ?elapsed,
		"jwks fetch complete"
	);

	Ok(HttpFetch { exchange, jwks: Some(Arc::new(jwks)), etag, last_modified })
}

/// Extract cache-control header as string for diagnostics.
pub fn cache_control_header(headers: &HeaderMap) -> Option<String> {
	headers.get(CACHE_CONTROL).and_then(|value| value.to_str().ok()).map(|s| s.to_string())
}
