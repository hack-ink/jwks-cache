<div align="center">

# jwks-cache

High-performance async JWKS cache with ETag revalidation, early refresh, and multi-tenant support — built for modern Rust identity systems.

[![License](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Docs](https://img.shields.io/docsrs/jwks-cache)](https://docs.rs/jwks-cache)
[![Rust](https://github.com/hack-ink/jwks-cache/actions/workflows/rust.yml/badge.svg?branch=main)](https://github.com/hack-ink/jwks-cache/actions/workflows/rust.yml)
[![Release](https://github.com/hack-ink/jwks-cache/actions/workflows/release.yml/badge.svg)](https://github.com/hack-ink/jwks-cache/actions/workflows/release.yml)
[![GitHub tag (latest by date)](https://img.shields.io/github/v/tag/hack-ink/jwks-cache)](https://github.com/hack-ink/jwks-cache/tags)
[![GitHub last commit](https://img.shields.io/github/last-commit/hack-ink/jwks-cache?color=red&style=plastic)](https://github.com/hack-ink/jwks-cache)
[![GitHub code lines](https://tokei.rs/b1/github/hack-ink/jwks-cache)](https://github.com/hack-ink/jwks-cache)

</div>

## Table of Contents

- [Why jwks-cache?](#why-jwks-cache)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Validating Tokens](#validating-tokens)
- [Registry Configuration](#registry-configuration)
- [Observability](#observability)
- [Persistence & Warm Starts](#persistence--warm-starts)
- [Development](#development)
- [Support](#support)
- [Acknowledgements](#acknowledgements)
- [License](#license)

## Why jwks-cache?

- **HTTP-aware caching**: honours `Cache-Control`, `Expires`, `ETag`, and `Last-Modified` headers via `http-cache-semantics`, so refresh cadence tracks the upstream contract instead of guessing TTLs.
- **Resilient refresh loop**: background workers use single-flight guards, exponential backoff with jitter, and bounded stale-while-error windows to minimise pressure on identity providers.
- **Multi-tenant registry**: isolate registrations per tenant, enforce HTTPS, and restrict redirect targets with domain allowlists or SPKI pinning.
- **Built-in observability**: metrics, traces, and status snapshots are emitted with tenant/provider labels to simplify debugging and SLO tracking.
- **Optional persistence**: Redis-backed snapshots allow the cache to warm-start without stampeding third-party JWKS endpoints after deploys or restarts.

## Installation

Add the crate to your project and enable optional integrations as needed:

```toml
# Cargo.toml
[dependencies]
# Drop `redis` if persistence is unnecessary.
jwks-cache = { version = "0.1", features = ["redis"] }
jsonwebtoken = { version = "10.1" }
metrics = { version = "0.24" }
reqwest = { version = "0.12", features = ["http2", "json", "rustls-tls", "stream"] }
tracing = { version = "0.1" }
tokio = { version = "1.48", features = ["macros", "rt-multi-thread", "sync", "time"] }
```

The crate is fully async and designed for the Tokio multi-threaded runtime.

## Quick Start

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
	tracing_subscriber::fmt::init();
	// Optional Prometheus exporter (requires the `prometheus` feature).
	jwks_cache::install_default_exporter()?;

	let registry = jwks_cache::Registry::builder()
		.require_https(true)
		.add_allowed_domain("tenant-a.auth0.com")
		.with_redis_client(redis::Client::open("redis://127.0.0.1/")?)
		.build();
	let mut registration = jwks_cache::IdentityProviderRegistration::new(
		"tenant-a",
		"auth0",
		"https://tenant-a.auth0.com/.well-known/jwks.json",
	)?;

	registration.stale_while_error = std::time::Duration::from_secs(90);
	registry.register(registration).await?;

	let jwks = registry.resolve("tenant-a", "auth0", None).await?;

	println!("Fetched {} keys.", jwks.keys.len());

	// No-op unless the `redis` feature is enabled.
	registry.persist_all().await?;

	Ok(())
}
```

## Validating Tokens

Use the registry to resolve a `kid` and build a `DecodingKey` for `jsonwebtoken`:

```rust
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use jwks_cache::Registry;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Claims {
	sub: String,
	exp: usize,
	aud: Vec<String>,
}

async fn verify(registry: &Registry, token: &str) -> Result<Claims, Box<dyn std::error::Error>> {
	let header = jsonwebtoken::decode_header(token)?;
	let kid = header.kid.ok_or("token is missing a kid claim")?;
	let jwks = registry.resolve("tenant-a", "auth0", Some(&kid)).await?;
	let jwk = jwks.find(&kid).ok_or("no JWKS entry found for kid")?;
	let decoding_key = DecodingKey::from_jwk(jwk)?;
	let mut validation = Validation::new(header.alg);

	validation.set_audience(&["api://default"]);

	let token = jsonwebtoken::decode::<Claims>(token, &decoding_key, &validation)?;

	Ok(token.claims)
}
```

The optional third argument to `Registry::resolve` lets you pass the `kid` up front, enabling cache hits even when providers rotate keys frequently.

## Registry Configuration

`Registry` keeps tenant/provider state isolated while applying consistent guardrails. The most relevant knobs on `IdentityProviderRegistration` are:

| Field                | Purpose                                          | Default                                                                                       |
| -------------------- | ------------------------------------------------ | --------------------------------------------------------------------------------------------- |
| `refresh_early`      | Proactive refresh lead time before TTL expiry.   | `30s` (overridable globally via `RegistryBuilder::default_refresh_early`)                     |
| `stale_while_error`  | Serve cached payloads while refreshes fail.      | `60s` (overridable via `default_stale_while_error`)                                           |
| `min_ttl`            | Floor applied to upstream cache directives.      | `30s`                                                                                         |
| `max_ttl`            | Cap applied to upstream TTLs.                    | `24h`                                                                                         |
| `max_response_bytes` | Maximum JWKS payload size accepted.              | `1_048_576 bytes`                                                                             |
| `negative_cache_ttl` | Optional TTL for failed upstream fetches.        | Disabled (`0s`)                                                                               |
| `max_redirects`      | Upper bound on HTTP redirects while fetching.    | `3` (hard limit `10`)                                                                         |
| `prefetch_jitter`    | Randomised offset applied to refresh scheduling. | `5s`                                                                                          |
| `retry_policy`       | Exponential backoff configuration for fetches.   | Initial attempt + 2 retries, 250 ms → 2 s backoff, 3 s per attempt, 8 s deadline, full jitter |
| `pinned_spki`        | SHA-256 SPKI fingerprints for TLS pinning.       | Empty                                                                                         |

### Multi-tenant operations

- `register` / `unregister` keep provider state scoped to each tenant.
- `resolve` serves cached JWKS payloads with per-tenant metrics tagging.
- `refresh` triggers an immediate background refresh without waiting for TTL expiry.
- `provider_status` and `all_statuses` expose lifecycle state, expiry, and error counters, plus hit rates and status metrics when the `metrics` feature is enabled.

### Security controls

- `RegistryBuilder::require_https(true)` (default) enforces HTTPS for every registration.
- Domain allowlists can be applied globally (`add_allowed_domain`) or per registration (`allowed_domains`).
- Provide `pinned_spki` values (base64 SHA-256) to guard against certificate substitution.

### Feature flags

- The `redis` feature enables Redis-backed snapshots for `persist_all` and `restore_from_persistence`. When disabled, these methods are cheap no-ops so lifecycle code can stay shared.
- The `metrics` feature enables metrics emission through the `metrics` facade.
- The `prometheus` feature enables `install_default_exporter` to install the bundled Prometheus recorder (implies `metrics`).
- The default features include `prometheus` and `metrics`; disable them with `default-features = false`.

## Observability

- Metrics emitted via the `metrics` facade (requires the `metrics` feature) include `jwks_cache_requests_total`, `jwks_cache_hits_total`, `jwks_cache_misses_total`, `jwks_cache_stale_total`, `jwks_cache_refresh_total`, `jwks_cache_refresh_errors_total`, and the `jwks_cache_refresh_duration_seconds` histogram.
- The `install_default_exporter` function installs the bundled Prometheus recorder (`metrics-exporter-prometheus`) and exposes a `PrometheusHandle` for HTTP servers to serve `/metrics` (requires the `prometheus` feature).
- Every cache operation is instrumented with `tracing` spans keyed by tenant and provider identifiers, making it easy to correlate logs, traces, and metrics.

## Persistence & Warm Starts

Enable the `redis` feature to persist JWKS payloads between deploys:

```rust
let registry = jwks_cache::Registry::builder()
	.require_https(true)
	.add_allowed_domain("tenant-a.auth0.com")
	.with_redis_client(redis::Client::open("redis://127.0.0.1/")?)
	.build();

// During startup:
registry.restore_from_persistence().await?;
// On graceful shutdown:
registry.persist_all().await?;
```

Snapshots store the JWKS body, validators, and expiry metadata, keeping cold starts off identity provider rate limits.

## Development

- `cargo fmt`
- `cargo clippy --all-targets --all-features`
- `cargo test`
- `cargo test --features redis` (integration coverage for Redis persistence)

Integration tests rely on `wiremock` to exercise HTTP caching behaviour, retries, and stale-while-error semantics.

## Support Me

If you find this project helpful and would like to support its development, you can buy me a coffee!

Your support is greatly appreciated and motivates me to keep improving this project.

- **Fiat**
    - [Ko-fi](https://ko-fi.com/hack_ink)
    - [爱发电](https://afdian.com/a/hack_ink)
- **Crypto**
    - **Bitcoin**
        - `bc1pedlrf67ss52md29qqkzr2avma6ghyrt4jx9ecp9457qsl75x247sqcp43c`
    - **Ethereum**
        - `0x3e25247CfF03F99a7D83b28F207112234feE73a6`
    - **Polkadot**
        - `156HGo9setPcU2qhFMVWLkcmtCEGySLwNqa3DaEiYSWtte4Y`

Thank you for your support!

## Appreciation

We would like to extend our heartfelt gratitude to the following projects and contributors:

Grateful for the Rust community and the maintainers of `reqwest`, `http-cache-semantics`, `metrics`, `redis`, and `tracing`, whose work makes this cache possible.

## Additional Acknowledgements

- TODO

<div align="right">

### License

<sup>Licensed under [GPL-3.0](LICENSE).</sup>

</div>
