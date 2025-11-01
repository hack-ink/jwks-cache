//! HTTP cache semantics integration helpers.

// crates.io
use http::{Method, Request, Response, Uri};
use http_cache_semantics::{AfterResponse, CachePolicy};
// self
use crate::{_prelude::*, http::client::HttpExchange, registry::IdentityProviderRegistration};

/// Freshness evaluation derived from HTTP headers and registry policy.
#[derive(Clone, Debug)]
pub struct Freshness {
	/// Effective time-to-live allowed for the JWKS payload.
	/// Clamped TTL in seconds, derived from HTTP Cache-Control and registry bounds.
	pub ttl: Duration,
	/// HTTP cache policy describing future request handling.
	pub policy: CachePolicy,
}

/// Result of applying conditional revalidation.
#[derive(Debug)]
pub struct Revalidation {
	/// Freshness information resulting from the revalidation exchange.
	pub freshness: Freshness,
	/// Response synthesized from the revalidation outcome.
	pub response: Response<()>,
	/// Flag indicating the upstream representation changed.
	pub modified: bool,
}

/// Build a baseline HTTP request for the provider JWKS endpoint.
pub fn base_request(registration: &IdentityProviderRegistration) -> Result<Request<()>> {
	let uri = parse_uri(registration)?;

	Request::builder()
		.method(Method::GET)
		.uri(uri)
		.header("accept", "application/json")
		.body(())
		.map_err(Error::from)
}

/// Evaluate HTTP cache semantics to determine TTL for the fetched JWKS document.
pub fn evaluate_freshness(
	registration: &IdentityProviderRegistration,
	exchange: &HttpExchange,
) -> Result<Freshness> {
	let policy = CachePolicy::new(&exchange.request, &exchange.response);
	let storable = policy.is_storable();
	let ttl = if storable {
		clamp_ttl(
			policy.time_to_live(SystemTime::now()),
			registration.min_ttl,
			registration.max_ttl,
		)
	} else {
		registration.min_ttl
	};

	tracing::debug!(ttl=?ttl, storable, "evaluated freshness");

	Ok(Freshness { ttl, policy })
}

/// Evaluate cache semantics for a conditional revalidation attempt.
pub fn evaluate_revalidation(
	registration: &IdentityProviderRegistration,
	policy: &CachePolicy,
	request: &Request<()>,
	response: &Response<()>,
) -> Result<Revalidation> {
	let now = SystemTime::now();
	let outcome = policy.after_response(request, response, now);
	let (policy, parts, modified) = match outcome {
		AfterResponse::NotModified(policy, parts) => (policy, parts, false),
		AfterResponse::Modified(policy, parts) => (policy, parts, true),
	};
	let response = Response::from_parts(parts, ());
	let ttl = clamp_ttl(policy.time_to_live(now), registration.min_ttl, registration.max_ttl);

	Ok(Revalidation { freshness: Freshness { ttl, policy }, response, modified })
}

fn parse_uri(registration: &IdentityProviderRegistration) -> Result<Uri> {
	registration.jwks_url.as_str().parse::<Uri>().map_err(|err| Error::Validation {
		field: "jwks_url",
		reason: format!("Failed to convert URL to http::Uri: {err}."),
	})
}

fn clamp_ttl(ttl: Duration, min: Duration, max: Duration) -> Duration {
	if ttl < min {
		min
	} else if ttl > max {
		max
	} else {
		ttl
	}
}

#[cfg(test)]
mod tests {
	// crates.io
	use http::{
		StatusCode,
		header::{CACHE_CONTROL, ETAG},
	};
	use http_cache_semantics::BeforeRequest;
	// self
	use super::*;

	fn make_registration() -> IdentityProviderRegistration {
		IdentityProviderRegistration::new(
			"tenant",
			"provider",
			"https://example.com/.well-known/jwks.json",
		)
		.expect("registration")
	}

	#[test]
	fn clamps_ttl_to_registration_bounds() {
		let mut registration = make_registration();

		registration.min_ttl = Duration::from_secs(30);
		registration.max_ttl = Duration::from_secs(60);

		let request = base_request(&registration).expect("request");
		let response = Response::builder()
			.status(StatusCode::OK)
			.header(CACHE_CONTROL, "max-age=5")
			.body(())
			.expect("response");
		let exchange = HttpExchange::new(request, response, Duration::from_millis(12));
		let freshness = evaluate_freshness(&registration, &exchange).expect("freshness");

		assert_eq!(freshness.ttl, Duration::from_secs(30));
	}

	#[test]
	fn adds_etag_to_conditional_revalidation_headers() {
		let mut registration = make_registration();

		registration.require_https = false;
		registration.min_ttl = Duration::from_secs(1);
		registration.max_ttl = Duration::from_secs(10);

		let request = base_request(&registration).expect("request");
		let response = Response::builder()
			.status(StatusCode::OK)
			.header(CACHE_CONTROL, "max-age=1")
			.header(ETAG, "\"jwks-tag\"")
			.body(())
			.expect("response");
		let exchange = HttpExchange::new(request.clone(), response, Duration::from_millis(8));
		let freshness = evaluate_freshness(&registration, &exchange).expect("freshness");
		let request = base_request(&registration).expect("request");
		let decision =
			freshness.policy.before_request(&request, SystemTime::now() + Duration::from_secs(5));

		match decision {
			BeforeRequest::Stale { request, .. } => {
				let if_none_match = request.headers.get("if-none-match");

				assert_eq!(
					if_none_match.and_then(|value| value.to_str().ok()),
					Some("\"jwks-tag\"")
				);
			},
			BeforeRequest::Fresh(_) => {
				panic!("expected stale decision triggering conditional headers")
			},
		}
	}
}
