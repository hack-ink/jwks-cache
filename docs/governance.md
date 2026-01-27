# Documentation Governance

Purpose: Define how documentation is organized, updated, and kept consistent across this
repository.

## Principles

- Write documentation that is clear, concise, retrieval-friendly, and LLM-first.
- Keep contracts and invariants in `docs/spec/`; keep runbooks and how-to guidance in
  `docs/guide/`.
- Avoid duplicating authoritative content. Link to the source of truth instead.

## Document classes and ownership

| Class | Location | Source of truth for | Update trigger |
| --- | --- | --- | --- |
| Spec | `docs/spec/` | Cache behavior, registry/persistence contracts, security invariants | Any behavior or configuration change |
| Operational docs | `docs/guide/` | Runbooks, development workflows, maintenance | When operating procedures change |

## Placement rules

- If it defines a contract, it belongs in `docs/spec/`.
- If it explains how to run or maintain a system, it belongs in `docs/guide/`.
- Avoid temporary plan documents. Capture decisions in specs or guides instead.
- Module documentation must live under `docs/guide/` and be linked from `docs/guide/index.md`.
  Do not add module-level README files.
- Do not duplicate the same content in both spec and guide files. Spec defines what must be true;
  guide explains how to operate or implement it. When in doubt, link to the source of truth.

## Canonical entry points

- Repository overview: `README.md` (the only README in the repository).
- Specs: `docs/spec/index.md`.
- Operational docs: `docs/guide/index.md`.
- Unified documentation index: `docs/index.md`.

## LLM reading guidance

When answering questions about system behavior:

1. Read `AGENTS.md` for tool and scope rules.
2. Use `docs/spec/index.md` for contracts and invariants (then follow linked specs).
3. Use `docs/guide/index.md` for runbooks and operational workflows.

## Update workflow

- Behavior or schema change: update the relevant `docs/spec/` doc.
- Procedure change: update the relevant `docs/guide/` guide.
- Avoid copying long sections between documents. Link instead.

## Naming conventions

- Spec files use descriptive `snake_case` names. Avoid numeric prefixes.
- Guide files use descriptive `snake_case` names within their category folders
  (`development/`). Add new folders only when needed and link them from the guide index.
