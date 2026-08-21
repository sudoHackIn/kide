# Dependency artifact-cache contract v1

- Status: proposed
- Dependency identity version: `1`
- Shared blob-container format: `1`
- Graph payload format: `ArtifactBlobLayout` version `1`
- Owner: KIDE Core

## Boundary

Two catalogs have different ownership and retention rules:

```text
project index (SQLite, per workspace)        shared blob cache (user/workspace scoped)
-------------------------------------        -----------------------------------------
current resolved dependency identity  --->   immutable blob keyed by full identity
component/source-unit routing data           bytes, header and physical access metadata
```

The project artifact catalog says *which identity the latest complete project
manifest resolved*. The shared blob catalog says *whether immutable bytes for
that identity exist*. Neither is a source of build-tool truth: workers resolve
dependencies; Core only validates, persists, and reconciles their normalized
results. A project catalog must never store a worker-local artifact path.

## Resolved dependency identity

The canonical v1 JSON serialization is the declaration-ordered serialization
of `ResolvedDependencyIdentity` in Core. It is the input to the opaque
`sha256:` blob key, not a user-facing package coordinate:

```json
{
  "identity_version": 1,
  "ecosystem": "maven",
  "canonical_coordinate": "org.example:library",
  "resolved_version": "1.2.3",
  "content": "sha256:…",
  "context": "sha256:…",
  "provenance": {
    "backend": "kide-kotlin-jvm",
    "backend_version": "…",
    "protocol_version": 3,
    "analysis_options": "sha256:…"
  },
  "canonical_schema_version": 1,
  "blob_format_version": 1
}
```

`canonical_coordinate` and `resolved_version` are optional. An artifact from a
local file, a platform module, or an unregistered repository is represented by
their absence, never by a fabricated coordinate. Its exact content digest,
context, provenance, and versions still make it safely cacheable.

Until a worker reports registry metadata, Core uses `ecosystem: "unknown"` and
both optional values absent. This is deliberately compatible only with another
equally unattributed artifact; task `kide-yqp.2` must carry resolved metadata
into persisted descriptors.

## Exact cache-hit rule

Core may load a blob only when every identity field matches exactly, including:

- ecosystem, coordinate presence/value, and resolved-version presence/value;
- source-content digest and component/build context digest;
- backend, backend version, worker protocol, and analysis-options digest;
- canonical-schema and artifact-payload format versions.

Changing any field is a miss. Blob byte length and SHA-256 received from the
worker are verified before publication; the immutable cache header binds the
same identity key. A missing/corrupt/truncated blob is a miss plus a safe
rebuild, never a semantic answer from stale bytes.

## SQLite catalog v1 (implemented by `kide-yqp.2`)

Core will migrate each project index transactionally to a versioned catalog
with the following logical records:

| Record | Key | Required data |
| --- | --- | --- |
| `project_artifact_catalog` | workspace + source unit | full dependency identity, current component, symbol routing metadata |
| `shared_blob_catalog` | opaque blob key | identity serialization, byte length, creation/access timestamps, state |
| `project_blob_reference` | project catalog row + blob key | current reference only; no local path |

The physical shared-cache directory remains outside a project SQLite database.
Its catalog is Core-owned and format-versioned independently from the project
index. Publication order is: verify staged bytes, atomically publish immutable
blob, then atomically publish project/catalog references. A crash before the
last step leaves only an unreferenced blob, which reconciliation may remove;
it never leaves a project row pointing at unverified bytes.

Readers accept only the identity and blob-format versions they understand.
Unknown newer versions are incompatible cache misses, not migration guesses.
Old key layouts are not rewritten in place: their references are dropped on
the next project-catalog replacement and fresh bytes are published under v1.

## Retention and reconciliation

Project replacement deletes references absent from the latest complete
manifest. It does not directly delete a shared blob, because another workspace
may still reference it. Shared-cache reconciliation removes only blobs that
are missing, corrupt, unreferenced after a grace period, or explicitly evicted
by a future byte-budget policy. Access metadata is advisory and cannot turn an
identity mismatch into a hit.

## Acceptance tests

Core tests pin v1 JSON serialization, preserve the absent-coordinate case,
and prove that context or analysis-options changes miss the cache. The
persistence task adds migration, restart-recovery, concurrent publication, and
catalog-reference tests against this contract.
