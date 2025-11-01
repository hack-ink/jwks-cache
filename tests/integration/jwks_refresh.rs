//! Integration tests for JWKS refresh and caching behaviour.

// std
use std::{sync::Arc, time::Duration};
// crates.io
use jwks_cache::{IdentityProviderRegistration, Registry, Result};
use wiremock::{
	Mock, MockServer, ResponseTemplate,
	matchers::{method, path},
};

const JWKS_BODY: &str = r#"{
    "keys": [
        {
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "kid": "primary",
            "n": "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCkqKywtLi8wMTIzNDU2Nzg5Ojs8PT4_QEFCQ0RFRkdISUpLTE1OT1BRUlNUVVZXWFlaW1xdXl9gYWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXp7fH1-f4A",
            "e": "AQAB"
        }
    ]
}"#;

#[tokio::test]
async fn caches_jwks_after_initial_fetch() -> Result<()> {
	let _ = tracing_subscriber::fmt::try_init();

	let server = MockServer::start().await;
	let jwks_path = "/.well-known/jwks.json";

	Mock::given(method("GET"))
		.and(path(jwks_path))
		.respond_with(
			ResponseTemplate::new(200)
				.set_body_string(JWKS_BODY)
				.insert_header("content-type", "application/json")
				.insert_header("cache-control", "public, max-age=60"),
		)
		.expect(1)
		.mount(&server)
		.await;

	let registration = IdentityProviderRegistration::new(
		"tenant-a",
		"auth0",
		format!("{}{}", server.uri(), jwks_path),
	)
	.expect("registration")
	.with_require_https(false);

	let registry = Registry::builder().require_https(false).build();
	registry.register(registration).await?;

	let first = registry.resolve("tenant-a", "auth0", None).await?;
	let second = registry.resolve("tenant-a", "auth0", None).await?;

	assert_eq!(first.keys.len(), 1);
	assert_eq!(second.keys.len(), 1);
	assert!(Arc::ptr_eq(&first, &second));

	server.verify().await;
	Ok(())
}

#[tokio::test]
async fn revalidates_conditionally_and_serves_stale_on_error() -> Result<()> {
	let _ = tracing_subscriber::fmt::try_init();

	let server = MockServer::start().await;
	let jwks_path = "/.well-known/jwks.json";

	let initial = ResponseTemplate::new(200)
		.set_body_string(JWKS_BODY)
		.insert_header("content-type", "application/json")
		.insert_header("cache-control", "public, max-age=1")
		.insert_header("etag", "\"v1\"");

	let revalidate = ResponseTemplate::new(304)
		.insert_header("cache-control", "public, max-age=1")
		.insert_header("etag", "\"v1\"");

	let failure = ResponseTemplate::new(500).set_delay(Duration::from_millis(10));

	let initial_template = initial.clone();
	let revalidate_template = revalidate.clone();
	let failure_template = failure.clone();
	let request_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
	let counter_handle = request_counter.clone();

	Mock::given(method("GET"))
		.and(path(jwks_path))
		.respond_with(move |request: &wiremock::Request| {
			let idx = counter_handle.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
			match idx {
				0 => initial_template.clone(),
				1 => {
					assert!(
						request.headers.contains_key("if-none-match"),
						"conditional header missing"
					);
					revalidate_template.clone()
				},
				_ => failure_template.clone(),
			}
		})
		.mount(&server)
		.await;

	let mut registration = IdentityProviderRegistration::new(
		"tenant-a",
		"auth0",
		format!("{}{}", server.uri(), jwks_path),
	)
	.expect("registration")
	.with_require_https(false);
	registration.refresh_early = Duration::from_secs(55);
	registration.stale_while_error = Duration::from_secs(120);
	registration.prefetch_jitter = Duration::ZERO;

	let registry = Registry::builder().require_https(false).build();
	registry.register(registration).await?;

	let first = registry.resolve("tenant-a", "auth0", None).await?;

	tokio::time::sleep(Duration::from_secs(6)).await;
	let second = registry.resolve("tenant-a", "auth0", None).await?;
	assert!(Arc::ptr_eq(&first, &second), "304 should reuse cached JWKS");

	registry.refresh("tenant-a", "auth0").await?;
	tokio::time::sleep(Duration::from_secs(1)).await;
	let third = registry.resolve("tenant-a", "auth0", None).await?;
	assert_eq!(third.keys.len(), first.keys.len(), "stale entry retains cached keyset");

	server.verify().await;
	Ok(())
}
