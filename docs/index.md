# Documentation Index

Purpose: Provide the canonical entry point and reading order for repository documentation.

Audience: Engineers and LLMs reading the repository.

## How to use this index

- Read `AGENTS.md` first for tooling and scope constraints.
- Use `docs/spec/index.md` for contracts and system behavior.
- Use `docs/guide/index.md` for procedures, troubleshooting, and operational guidance.
- Use `docs/governance.md` for documentation rules and placement.
- If a referenced document does not exist, state that it is missing.

## Reading order

1. `AGENTS.md`
2. `docs/governance.md`
3. `docs/spec/index.md`
4. `docs/guide/index.md`

## Document sets

### Specifications (normative)

- Location: `docs/spec/`.
- Scope: Architecture, cache behavior, registry contracts, persistence, and security invariants.

### Operational guides

- Location: `docs/guide/`.
- Scope: Development workflows, style rules, and troubleshooting for this crate.

### Governance

- Location: `docs/governance.md`.
