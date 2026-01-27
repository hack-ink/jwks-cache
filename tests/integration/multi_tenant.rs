//! Integration coverage for multi-tenant registry operations and status inspection.

// std
use std::{sync::Arc, time::Duration};
// crates.io
use jwks_cache::{Error, IdentityProviderRegistration, ProviderState, Registry, Result};
use url::Url;
use wiremock::{
	Mock, MockServer, ResponseTemplate,
	matchers::{method, path},
};

const JWKS_A: &str = r#"{
    "keys": [
        {
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "kid": "tenant-a",
            "n": "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCkqKywtLi8wMTIzNDU2Nzg5Ojs8PT4_QEFCQ0RFRkdISUpLTE1OT1BRUlNUVVZXWFlaW1xdXl9gYWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXp7fH1-f4A",
            "e": "AQAB"
        }
    ]
}"#;

const JWKS_B: &str = r#"{
    "keys": [
        {
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "kid": "tenant-b",
            "n": "AQABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj9AQUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVpbXF1eX2BhYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5ent8fX5_gA",
            "e": "AQAB"
        }
    ]
}"#;

#[tokio::test]
async fn multi_tenant_registry_operations_and_status() -> Result<()> {
	let _ = tracing_subscriber::fmt::try_init();

	let server = MockServer::start().await;
	let path_a = "/tenant-a/.well-known/jwks.json";
	let path_b = "/tenant-b/.well-known/jwks.json";

	Mock::given(method("GET"))
		.and(path(path_a))
		.respond_with(
			ResponseTemplate::new(200)
				.set_body_string(JWKS_A)
				.insert_header("cache-control", "public, max-age=30")
				.insert_header("content-type", "application/json"),
		)
		.expect(1..)
		.mount(&server)
		.await;

	Mock::given(method("GET"))
		.and(path(path_b))
		.respond_with(
			ResponseTemplate::new(200)
				.set_body_string(JWKS_B)
				.insert_header("cache-control", "public, max-age=45")
				.insert_header("content-type", "application/json"),
		)
		.expect(1..)
		.mount(&server)
		.await;

	let base = Url::parse(&server.uri()).expect("mock url");
	let host = base.host_str().expect("host present").to_ascii_lowercase();

	let registry =
		Registry::builder().require_https(false).add_allowed_domain(host.clone()).build();

	let reg_a = IdentityProviderRegistration::new(
		"tenant-a",
		"primary",
		base.join(path_a).expect("join path"),
	)
	.expect("registration")
	.with_require_https(false);
	let reg_b = IdentityProviderRegistration::new(
		"tenant-b",
		"secondary",
		base.join(path_b).expect("join path"),
	)
	.expect("registration")
	.with_require_https(false);

	registry.register(reg_a).await?;
	registry.register(reg_b).await?;

	let first = registry.resolve("tenant-a", "primary", None).await?;
	let second = registry.resolve("tenant-b", "secondary", None).await?;
	assert_eq!(first.keys.len(), 1);
	assert_eq!(second.keys.len(), 1);

	// Subsequent hit should reuse cached payload and emit hit metrics.
	let repeat = registry.resolve("tenant-a", "primary", None).await?;
	assert!(Arc::ptr_eq(&first, &repeat), "cache should reuse JWKS for tenant-a");

	let status_a = registry.provider_status("tenant-a", "primary").await?;
	assert_eq!(status_a.tenant_id, "tenant-a");
	assert_eq!(status_a.provider_id, "primary");
	assert!(
		matches!(status_a.state, ProviderState::Ready | ProviderState::Refreshing),
		"expected ready or refreshing state, got {:?}",
		status_a.state
	);
	assert!(status_a.last_refresh.is_some(), "last refresh timestamp missing");
	assert!(status_a.next_refresh.is_some(), "next refresh timestamp missing");
	#[cfg(feature = "metrics")]
	{
		assert!(
			status_a.hit_rate >= 0.5 && status_a.hit_rate <= 1.0,
			"unexpected hit rate {}",
			status_a.hit_rate
		);
		assert!(
			status_a.metrics.iter().any(|metric| metric.name == "jwks_cache_hits_total"),
			"hits counter missing from status metrics"
		);
	}

	let statuses = registry.all_statuses().await;
	assert_eq!(statuses.len(), 2, "expected two provider statuses");

	assert!(registry.unregister("tenant-b", "secondary").await?, "expected provider removal");
	let err = registry.resolve("tenant-b", "secondary", None).await.unwrap_err();
	assert!(matches!(err, Error::NotRegistered { .. }));

	// Registering a provider outside the global allowlist should fail.
	let blocked_registration = IdentityProviderRegistration::new(
		"tenant-c",
		"blocked",
		"https://untrusted.example.com/jwks.json",
	)
	.expect("registration");
	let result = registry.register(blocked_registration).await;
	assert!(
		matches!(result, Err(Error::Security(_)) | Err(Error::Validation { .. })),
		"registration should fail due to allowlist enforcement"
	);

	// Ensure persistence hooks are no-ops when not configured.
	registry.persist_all().await?;

	// Allow background tasks to complete before finishing test.
	tokio::time::sleep(Duration::from_millis(100)).await;
	server.verify().await;
	Ok(())
}
