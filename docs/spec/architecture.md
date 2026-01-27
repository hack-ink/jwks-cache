# Architecture

Purpose: Define system architecture, key decisions, and repository boundaries for jwks-cache.

Audience: Engineers and LLMs reading the canonical system specification.

Scope: JWKS caching behavior, registry lifecycle, HTTP semantics, persistence, metrics, security, and repository layout.

## Platform targets

- Rust library crate designed for async runtimes.
- No OS-specific code; consumers decide runtime integration.
- Optional Redis persistence is gated by the `redis` feature.

## Runtime and dependencies

- Async runtime: Tokio multi-thread runtime.
- HTTP client: Reqwest with Rustls TLS.
- Caching semantics: `http-cache-semantics` for `Cache-Control`, `ETag`, and `Last-Modified`.
- JWKS parsing: `jsonwebtoken::jwk::JwkSet`.

## Registry and cache lifecycle

- `Registry` owns tenant/provider registrations and per-provider cache managers.
- Each tenant/provider pair has a `CacheManager` that enforces single-flight refreshes.
- Cache states: `Empty`, `Loading`, `Ready`, `Refreshing`.
- Refresh cadence is driven by:
	- `refresh_early` lead time before expiry.
	- `stale_while_error` window when refresh fails.
	- `min_ttl` / `max_ttl` clamps on upstream cache directives.
	- `retry_policy` backoff strategy for refresh attempts.

## Persistence (optional)

- Enable the `redis` feature to persist snapshots between deploys.
- `Registry::restore_from_persistence` restores cache state on startup.
- `Registry::persist_all` captures JWKS payloads, validators, and expiry metadata on shutdown.

## Metrics and tracing

- Metrics flow through the `metrics` facade.
- `install_default_exporter` installs the bundled Prometheus recorder.
- Cache operations emit structured `tracing` spans keyed by tenant and provider identifiers.

## Security and validation

- HTTPS is required by default (`require_https = true`).
- Redirect allowlist via `allowed_domains`.
- Redirect depth is capped by `max_redirects`.
- Payload size guard via `max_response_bytes`.
- Optional TLS pinning via `pinned_spki` fingerprints.

## Repository layout (current)

```
.
├── docs/
│   ├── guide/                        # Operational guidance and development rules.
│   └── spec/                         # Normative system specifications.
├── src/                              # Library implementation.
├── tests/                            # Integration tests (wiremock).
├── Cargo.toml                        # Crate metadata and dependencies.
├── Makefile.toml                     # Use `cargo make fmt`, `cargo make lint`, `cargo make test`, `cargo make test-redis`.
├── README.md                         # Public-facing overview and usage.
└── rust-toolchain.toml               # Pinned Rust toolchain.
```

## Key boundaries

### `registry`

Owns registrations, validation, and coordinates cache managers.

### `cache`

Implements cache state, refresh scheduling, and single-flight behavior.

### `http`

Handles JWKS fetches, retry policies, and HTTP caching semantics.

### `metrics`

Captures per-provider metrics and exposes Prometheus-compatible exporters.

### `security`

Validates HTTPS requirements, allowed domains, and TLS pinning settings.
