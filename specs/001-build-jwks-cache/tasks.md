---
description: "Task list for Production JWKS Caching Library"
---

# Tasks: Production JWKS Caching Library

**Input**: Design documents from `/specs/001-build-jwks-cache/`
**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Tests are included where acceptance scenarios require automated validation of caching semantics, stale handling, and multi-tenant observability.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions
- Keep each task scope simple and maintainable; split work when clarity drops
- Write descriptions in formal English without slang or contractions
- Call out required MCP server usage instead of manual tooling when relevant

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and crate scaffolding

- [X] T001 Update dependencies and feature flags for reqwest, jsonwebtoken, http-cache-semantics, tracing, metrics, and optional redis in Cargo.toml
- [X] T002 Declare crate modules, feature gating, and public exports in src/lib.rs
- [X] T003 Initialize async testing scaffolding with module declarations in tests/integration/mod.rs and tests/unit/mod.rs

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core types and security utilities required across all user stories

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T004 Define shared Result alias, error enums, and error conversion glue in src/lib.rs
- [X] T005 Implement IdentityProviderRegistration, RetryPolicy, and PersistentSnapshot validation per data-model.md in src/registry.rs
- [X] T006 [P] Implement HTTPS enforcement, allowed domain checks, and SPKI pin validation helpers in src/security.rs

**Checkpoint**: Foundation ready — user story implementation can now begin in parallel

---

## Phase 3: User Story 1 - Serve JWT Verification Keys Reliably (Priority: P1) 🎯 MVP

**Goal**: Deliver cached JWKS responses so verification services avoid repeated upstream calls while keys remain valid.

**Independent Test**: Provision a JWKS endpoint with cache headers, point a verification client at the cache, and confirm repeat requests are served from the cache until revalidation is required.

### Tests for User Story 1 ⚠️

- [X] T007 [P] [US1] Add integration test covering cache hits and initial fetch in tests/integration/jwks_refresh.rs
- [X] T008 [P] [US1] Add unit tests for cache entry state transitions in tests/unit/cache_entry.rs

### Implementation for User Story 1

- [X] T009 [US1] Implement JWKSCacheEntry storage with ttl, validator, and key parsing in src/cache/entry.rs
- [X] T010 [US1] Implement CacheState finite state machine and concurrency guards in src/cache/state.rs
- [X] T011 [US1] Integrate http-cache-semantics to compute freshness and revalidation hints in src/http/semantics.rs
- [X] T012 [US1] Build reqwest JWKS fetch client with HTTPS enforcement and response size guard in src/http/client.rs
- [X] T013 [US1] Implement CacheManager initial fetch, caching, and read API in src/cache/manager.rs
- [X] T014 [US1] Expose resolver returning jsonwebtoken::jwk::JwkSet to callers in src/lib.rs

**Checkpoint**: User Story 1 is fully functional and testable independently

---

## Phase 4: User Story 2 - Tolerate Provider Changes and Outages (Priority: P2)

**Goal**: Keep verification online during key rotations and transient upstream failures via conditional revalidation, background refresh, and stale-while-error behaviour.

**Independent Test**: Simulate rotations, 304 responses, timeouts, and stale-while-error windows to confirm cached keys remain available with timely refreshes.

### Tests for User Story 2 ⚠️

- [X] T015 [P] [US2] Add integration test exercising conditional revalidation and stale-while-error fallback in tests/integration/jwks_refresh.rs
- [X] T016 [P] [US2] Add unit tests for TTL clamping and validator handling in tests/unit/http_semantics.rs

### Implementation for User Story 2

- [X] T017 [US2] Extend http-cache-semantics wrapper to send conditional headers and handle 304 responses in src/http/semantics.rs
- [X] T018 [US2] Implement retry and backoff strategy with jitter support in src/http/retry.rs
- [X] T019 [US2] Add background refresh scheduling, single-flight guards, and stale-while-error windowing in src/cache/manager.rs
- [X] T020 [US2] Implement manual refresh trigger aligning with /refresh contract endpoint in src/cache/manager.rs

**Checkpoint**: User Stories 1 and 2 operate independently with resilient refresh workflows

---

## Phase 5: User Story 3 - Operate Multi-Tenant Deployments with Visibility (Priority: P3)

**Goal**: Provide multi-tenant registration, observability, and optional persistence so operators can manage cache health at scale.

**Independent Test**: Register multiple providers, observe cache states, and verify telemetry surfaces tenant-level latency, hit rate, and stale usage metrics.

### Tests for User Story 3 ⚠️

- [X] T021 [P] [US3] Add integration test for multi-tenant registry operations and status inspection in tests/integration/multi_tenant.rs
- [X] T022 [P] [US3] Add unit tests asserting metric emission and label coverage in tests/unit/metrics.rs

### Implementation for User Story 3

- [X] T023 [US3] Implement tenant registry map with register, update, delete, and allowlist enforcement in src/registry.rs
- [X] T024 [US3] Expose ProviderStatus snapshot generation and registry query API in src/registry.rs
- [X] T025 [US3] Emit metrics counters, histograms, and gauges for cache health in src/metrics.rs
- [X] T026 [US3] Add optional Redis snapshot persistence behind the redis feature in src/registry.rs
- [X] T027 [US3] Surface control plane adapters matching jwks-cache.openapi.yaml responses in src/lib.rs

**Checkpoint**: All user stories function independently with operational insight

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Harden instrumentation, documentation, and developer ergonomics

- [X] T028 [P] Add tracing spans and structured logging for refresh and registry paths in src/cache/manager.rs
- [X] T029 [P] Update multi-tenant and persistence walkthrough in specs/001-build-jwks-cache/quickstart.md
- [X] T030 Refine public usage guidance and operational notes in README.md

---

## Dependencies & Execution Order

- **Setup (Phase 1)**: Unblocked; must complete before foundational work.
- **Foundational (Phase 2)**: Depends on Setup; blocks all user stories.
- **User Story 1 (Phase 3)**: Depends on Foundational; delivers MVP cache serving.
- **User Story 2 (Phase 4)**: Depends on User Story 1 components within CacheManager and HTTP semantics.
- **User Story 3 (Phase 5)**: Depends on User Story 1 for cache primitives and User Story 2 for resilient refresh guarantees.
- **Polish (Phase 6)**: Depends on all targeted user stories being complete.

**User Story Dependency Graph**: Setup → Foundational → US1 → US2 → US3 → Polish

---

## Parallel Execution Examples per Story

**User Story 1**
- Parallel tests: T007, T008
- Parallel implementations: T011, T012 once T009–T010 establish core types

**User Story 2**
- Parallel tests: T015, T016
- Parallel implementations: T017, T018 after CacheManager from T013 is available

**User Story 3**
- Parallel tests: T021, T022
- Parallel implementations: T023, T025 can progress separately once registry scaffolding exists

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1 (Setup)
2. Complete Phase 2 (Foundational)
3. Execute Phase 3 (User Story 1) to deliver reliable JWKS serving
4. Run tests from T007–T008 to validate cache behaviour before proceeding

### Incremental Delivery

1. Deliver MVP (US1) and validate against quickstart scenario
2. Layer User Story 2 to handle rotations and outages; validate via T015–T016
3. Add User Story 3 for multi-tenant operations and telemetry; validate via T021–T022
4. Finish with Phase 6 polish items for documentation and observability

### Parallel Team Strategy

1. Team collaborates on Setup and Foundational phases
2. After CacheManager scaffolding (T013), split efforts:
   - Developer A: finalize US1 implementation and tests
   - Developer B: extend refresh resilience (US2)
   - Developer C: build registry, metrics, and persistence (US3)
3. Reconvene for Polish tasks T028–T030 before release
