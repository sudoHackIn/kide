# Freshness and Incremental Index Contract v1

- Status: accepted
- Configuration schema version: `1`
- Checkpoint contract version: `1`
- Rust source of truth: `crates/kide-core/src/{configuration,freshness}.rs`

## Configuration

KIDE starts from built-in defaults, overlays the optional TOML file named by
`KIDE_GLOBAL_CONFIG`, then overlays `.kide/config.toml` at the workspace root.
Every existing layer is strictly parsed: unknown keys, invalid enum values and
unsupported schema versions are errors. Missing files are normal.

```toml
schema_version = 1
freshness_strategy = "fresh_only" # or "allow_stale"
```

`fresh_only` is the default. It returns `status: "stale"`, no semantic result,
and a retryable `stale_input` problem if any fact owner cannot be verified.
`allow_stale` may return the retained result, but must still return
`status: "stale"` and `metadata.freshness: "stale"`; stale data is never
reported as `ok` or `fresh`.

`kide init` writes both `.kide/config.toml` and `.kide/.gitignore`. The nested
ignore file ignores all generated KIDE state while retaining those two
declarative files for Git. The repository root `.gitignore` explicitly permits
these files; Git has no include directive for composing ignore files.

`kide status` exposes `configuration_schema_version` and
`freshness_strategy`, so automation can audit the policy that governs semantic
responses.

## Checkpoint invariants

A published index checkpoint has a monotonically increasing `generation` and
the following verified input coverage: workspace configuration, resolved
dependency graph, component contexts, source content/API inputs, artifact
catalog, and each artifact blob. A semantic response is `fresh` only when all
owners of its returned facts match the checkpoint for the published generation.

The state of each owner is one of `fresh`, `stale`, `indexing`, or `unknown`.
An unavailable lazy blob is distinct from an invalid blob: absence is
`unknown`/materializable; a failed content check is stale and cannot support a
fresh response. A newer generation supersedes an in-flight generation, which
must never publish its checkpoint or facts.
