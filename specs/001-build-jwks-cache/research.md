# Phase 0 Research

## HTTP Client Selection

- **Decision**: Use `reqwest` with the default Tokio executor, connection pooling, timeout configuration, and redirect limits aligned with security policy.
- **Rationale**: `reqwest` builds on `hyper` but exposes higher-level ergonomics, async TLS, request builders, and response streaming with backpressure. It supports header manipulation for conditional requests and integrates with `tower` middleware if needed. Choosing it reduces boilerplate for HTTPS enforcement and simplifies future retries and proxy handling.
- **Alternatives considered**: `hyper` (lower-level; would require custom redirect handling, decompression, and TLS wiring), `surf` (not as actively maintained, fewer features for conditional headers).

## HTTP Cache Semantics Handling

- **Decision**: Adopt the `http-cache-semantics` crate to interpret `Cache-Control`, `Expires`, `ETag`, and `Last-Modified` headers and compute freshness lifetimes.
- **Rationale**: The crate is a Rust port of the proven Node.js library, supports RFC 7234 semantics, and exposes APIs to derive TTL, revalidation requirements, and stale policies. Leveraging it avoids re-implementing nuanced HTTP caching rules and keeps behaviour aligned with industry standards.
- **Alternatives considered**: Manual header parsing (error-prone, higher maintenance), `reqwest-middleware` cache modules (designed for client-side caching rather than custom JWKS logic).

## Metrics Instrumentation Stack

- **Decision**: Use the `metrics` crate with optional exporters (`metrics-exporter-prometheus` or `metrics-util`) and wrap emitted series in helper functions for counters, histograms, and gauges.
- **Rationale**: `metrics` provides a stable facade compatible with multiple backends, supports label dimensions, and integrates cleanly with async code. It keeps instrumentation lightweight while enabling downstream Prometheus or OpenTelemetry bridges.
- **Alternatives considered**: Direct `opentelemetry` API usage (more verbose, higher setup cost), custom metrics registry (reinvents infrastructure).

## Tracing and Logging

- **Decision**: Leverage the `tracing` ecosystem (`tracing`, `tracing-futures`, `tracing-subscriber`) for structured spans around fetch, refresh, and registry operations.
- **Rationale**: `tracing` is the de facto async logging standard in Rust, supports span contexts, and integrates with `metrics` via shared fields. It enables correlation IDs for tenant operations without polluting log output with sensitive key material.
- **Alternatives considered**: `log`/`env_logger` (insufficient span support), ad-hoc logging macros (hard to standardize and filter).

## JWKS Parsing and Validation

- **Decision**: Use `jsonwebtoken::jwk::JwkSet` for parsing and validation, layering additional checks for key completeness (RSA `n`/`e`, EC `crv`/`x`/`y`, OKP `crv`/`x`) and deterministic selection logic.
- **Rationale**: Staying within the `jsonwebtoken` ecosystem ensures compatibility with existing JWT verification flows. Augmenting with structural validation closes gaps left for malformed provider responses and allows reuse of `jsonwebtoken` errors.
- **Alternatives considered**: Custom serde models (would duplicate maintenance), `ring`-based manual key parsing (unnecessary complexity).

## Async Runtime and Concurrency

- **Decision**: Standardize on the multi-threaded Tokio runtime, using `tokio::sync::RwLock` or `parking_lot::RwLock` for tenant registries, and `tokio::sync::Mutex` combined with single-flight guards for fetch coordination.
- **Rationale**: Tokio is industry-standard, integrates with `reqwest`, and offers monotonic timers via `tokio::time::Instant` for refresh scheduling. Combining RwLocks with single-flight patterns keeps reads lock-free while ensuring only one refresh per tenant occurs.
- **Alternatives considered**: `async-std` (fewer ecosystem integrations), `dashmap` (highly concurrent but adds complexity and potential blocking in async contexts).

## Persistence Strategy

- **Decision**: Provide an optional Redis-backed snapshot feature guarded by a crate feature flag, implemented with the `redis` crate and a connection pool (e.g., `bb8` or `deadpool-redis`).
- **Rationale**: Redis is widely available, suits key-value snapshots, and aligns with the spec’s optional persistence requirement. Feature-gating keeps the core library lightweight for deployments that do not need persistence.
- **Alternatives considered**: File-based snapshots (limited in distributed environments), custom trait without reference implementation (slows adoption).

## Test HTTP Mocking

- **Decision**: Use `wiremock-rs` for integration tests simulating JWKS servers with precise control over headers, response bodies, and timing.
- **Rationale**: `wiremock` supports async scenarios, dynamic responders, and verification of request patterns. It allows modeling redirect chains, timeouts, and header assertions required by the acceptance criteria.
- **Alternatives considered**: `httpmock` (synchronous focus, fewer async controls), spinning up bespoke Hyper servers (more boilerplate, harder to parameterize).
