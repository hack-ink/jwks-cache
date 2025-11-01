# Quickstart: JWKS Cache Library

## 1. Install

Add the crate to your project:

```toml
# Cargo.toml
[dependencies]
jwks-cache = { path = "../jwks-cache", features = ["redis"], optional = true }
jsonwebtoken = "9"
reqwest = { version = "0.12", features = ["json", "gzip"] }
tracing = "0.1"
metrics = "0.23"
```

> Disable the `redis` feature if persistence is not required.

## 2. Configure Runtime

Ensure your binary executes inside the multi-threaded Tokio runtime:

```rust
#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    jwks_cache::metrics::install_default_exporter()?;
    // ... application setup
    Ok(())
}
```

## 3. Create the Registry

```rust
use jwks_cache::{IdentityProviderRegistration, Registry};
use std::time::Duration;

let registry = Registry::builder()
    .require_https(true)
    .default_refresh_early(Duration::from_secs(45))
    .add_allowed_domain("tenant-a.auth0.com")
    .add_allowed_domain("tenant-b.okta.com")
    .build();

let tenant_a = IdentityProviderRegistration::new(
    "tenant-a",
    "auth0",
    "https://tenant-a.auth0.com/.well-known/jwks.json",
)?;

let mut tenant_b = IdentityProviderRegistration::new(
    "tenant-b",
    "okta",
    "https://tenant-b.okta.com/oauth2/default/v1/keys",
)?;
tenant_b.stale_while_error = Duration::from_secs(90);

registry.register(tenant_a).await?;
registry.register(tenant_b).await?;
```

## 4. Verify Tokens with Cached Keys

```rust
use jwks_cache::resolver::Resolver;
use jsonwebtoken::{decode, DecodingKey, Validation};

let jwks = registry.resolve("tenant-a", "auth0", None).await?;
let kid = "expected-kid";
let key = jwks.select(kid, None)?; // Deterministic selection from cache
let token_data = decode::<Claims>(jwt, &DecodingKey::from_rsa_components(&key.n, &key.e), &Validation::new(key.alg))?;
```

The registry returns cached keys within ≤5 ms P95 and revalidates in the background according to HTTP headers.

## 5. Observe Metrics, Traces, and Status

Metrics are emitted via the `metrics` facade. Couple them with the built-in Prometheus exporter:

```rust
jwks_cache::install_default_exporter()?; // exposes /metrics via prometheus handle

let status = registry.provider_status("tenant-a", "auth0").await?;
tracing::info!(
    tenant = %status.tenant_id,
    provider = %status.provider_id,
    state = ?status.state,
    hit_rate = %status.hit_rate,
    "cache status snapshot ready",
);
```

The status payload mirrors `jwks-cache.openapi.yaml`, exposing expiration timestamps, error counters, and summarised metrics per provider.

## 6. Handle Shutdown and Persistence

With the `redis` feature enabled, the registry can persist snapshots across restarts:

```rust
// Configure once during application bootstrap.
let registry = Registry::builder()
    .require_https(true)
    .add_allowed_domain("tenant-a.auth0.com")
    .with_redis_client(redis::Client::open("redis://127.0.0.1/")?)
    .build();

// On graceful shutdown:
registry.persist_all().await?;

// During startup:
registry.restore_from_persistence().await?;
```

If Redis is disabled, the persistence helpers become inexpensive no-ops, so the same lifecycle code can be shared across environments.

The cache enforces HTTPS-only URLs, redirect limits, and stale-while-error windows automatically.
