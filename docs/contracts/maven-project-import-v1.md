# Maven Project Import Contract v1

- Status: proposed
- Canonical schema version: `1` (unchanged)
- Worker protocol: `kide.worker.v1.ProjectManifestRequest` / `ProjectManifestResponse`
- Canonical records: `ProjectManifest`, `Component`, `SourceSet`, `DependencyEdge`
- Related architecture: [ADR 0001](../architecture/0001-cold-disposable-backend-workers.md)

## Purpose and boundary

This contract defines how a disposable Maven importer turns a Maven reactor
into KIDE's existing build-system-neutral `ProjectManifest`. It does not add a
Maven model, Maven coordinates, or Maven queries to Core. Maven parsing,
effective-model resolution, repository access, absolute JDK locations, and
artifact paths are worker-local inputs.

```text
pom.xml reactor + selected import inputs
              |
              v
  disposable Maven import adapter
              |
              v
  ProjectManifest(build_system = maven)
              |
              v
  persistent Core index and normal navigation/query APIs
```

The importer must produce a complete deterministic manifest or a structured
failure. It must never invent a partial component graph after Maven model
resolution has failed.

## Supported v1 subset

The initial implementation supports a local reactor rooted at the requested
workspace containing a root `pom.xml`, including:

- `jar` modules connected with `<modules>` and parent POM inheritance;
- Maven coordinates and properties needed by the selected effective model;
- standard Java roots: `src/main/java`, `src/test/java`;
- standard resources: `src/main/resources`, `src/test/resources`;
- Kotlin roots explicitly configured by `kotlin-maven-plugin`, including the
  conventional `src/main/kotlin` and `src/test/kotlin` paths;
- compile and test dependency scopes, including reactor-module dependencies;
- compiler release/source/target and Kotlin compiler-plugin configuration when
  it is available from the effective model.

The importer may use Maven's supported embedding/tooling APIs or a controlled
Maven invocation. It must not implement Maven semantics by loosely parsing XML
or scanning a local dependency directory.

## Import inputs and reproducibility

A v1 request carries only `workspace_root`; the following worker-local import
inputs are part of the resulting configuration fingerprint:

| Input | Rule |
| --- | --- |
| Root and module POM contents | Hash the effective set of POMs that contributes to the selected reactor. |
| Maven version | Record in worker-local diagnostics and incorporate into the fingerprint. |
| Active profiles | Use Maven's default activation plus explicitly supplied import profiles in a future request extension. v1 does not silently choose arbitrary profiles. |
| Properties/settings | Include effective property values that alter source roots, compiler settings, dependency graph, or repositories; credentials never leave the worker. |
| JDK/toolchain | Persist only portable version data in `Component.toolchain`; absolute paths remain worker-local. |
| Resolved artifacts | Use immutable content fingerprints in `Component.classpath` and `DependencyTarget::Artifact`; do not persist repository paths. |

Equivalent effective models must yield byte-stable component, dependency, and
source-set ordering. Changing an input above must make the relevant component
configuration fingerprint incompatible with previously indexed snapshots.

## Canonical manifest mapping

Every Maven module becomes one Core `Component`:

| Maven concept | Canonical representation |
| --- | --- |
| Reactor module | `Component { build_system: Maven }` |
| `groupId:artifactId` plus reactor-relative module path | Stable `ComponentId`; the exact encoding is importer-owned but must be deterministic and collision-free within the workspace. |
| Module directory | Workspace-relative `Component.root` |
| Main/test Java and Kotlin roots | `SourceSet { name: "main"/"test", test: false/true }` with sorted workspace-relative roots |
| Generated sources reported by Maven plugins | `SourceSet.generated_roots`; absent when a plugin cannot report them safely |
| Resource roots | Included in the source-set discovery inventory, but never submitted to Java/Kotlin semantic analysis as source files |
| Effective compiler settings | `Component.compiler_configuration` fingerprint |
| Java/Kotlin toolchain versions | `Component.toolchain`; Maven version occupies its portable `build_tool_version` field in v1 |
| Resolved external compile/test artifacts | Sorted content fingerprints in `Component.classpath` and artifact dependency edges |
| Reactor dependency | `DependencyTarget::Component` edge |
| External dependency | `DependencyTarget::Artifact` edge |

Dependency-edge `scope` values are normalized to lower-case Maven scopes
(`compile`, `runtime`, `test`, `provided`, `system`, or `import`). Optional and
excluded dependencies affect resolution and the configuration fingerprint; they
are not promoted to ad-hoc Core fields.

`Component.languages` is derived only from discovered semantic source roots.
Resources alone do not make a component Java or Kotlin.

## Source-set and language rules

The importer must not infer a language solely from a directory name. It may
declare Java or Kotlin only when an eligible source root contains a matching
source file or the effective model explicitly configures that language plugin.

For each root, the importer must:

1. canonicalize it under the workspace root;
2. reject any path escaping through `..` or a symlink outside the workspace;
3. classify it as source, generated, resource, or excluded before emitting it;
4. sort and deduplicate roots before calculating fingerprints;
5. keep test roots separate from main roots.

Generated source roots are indexed only after the importer establishes that
they exist and belong to the selected component. Missing generated output is
not an error by itself and must not cause Core to claim a complete generated
source inventory.

## Profiles and unsupported effective models

v1 supports Maven's ordinary default profile activation. A profile selected by
environment, filesystem, JDK, property, or explicit CLI input is acceptable
only when its selection inputs enter the configuration fingerprint.

The importer returns `unsupported_capability` rather than guessing for:

- a reactor whose effective model requires credentials or an interactive
  repository/login flow unavailable to the worker;
- a source-root/dependency mutation that cannot be observed from the effective
  model (for example arbitrary build-time scripting);
- a non-standard packaging or plugin-managed language source set with no
  stable reported roots;
- an unresolved profile selection whose alternatives would change semantic
  sources, compiler configuration, or classpath.

Malformed POMs, parent/model resolution failures, and dependency failures are
reported as structured import diagnostics. Core retains the last known complete
manifest until a replacement manifest commits atomically; it never mixes new
partial Maven facts with old component or artifact edges.

## Provenance and diagnostics

The response provenance identifies the importer backend, its version, protocol
version, and an analysis-options fingerprint that covers the import inputs.
Diagnostics may name Maven coordinates, POM-relative paths, scopes, profile
IDs, and sanitized repository identifiers. They must not expose credentials,
absolute JDK paths, local repository paths, or Maven settings secrets.

## Fixture matrix and acceptance proof

The implementation tasks following this contract must provide:

| Fixture | Required proof |
| --- | --- |
| Single-module Java | Standard main/test roots and compile dependency catalog mapping. |
| Multi-module Java reactor | Stable component IDs, reactor dependency edge, cross-module navigation. |
| Kotlin-enabled module | Explicit Kotlin Maven plugin roots, compiler fingerprint, mixed Java/Kotlin analysis where configured. |
| Incremental edit/delete | Only affected source units reanalyze; unchanged artifacts and modules reuse cached facts. |
| Malformed/unsupported model | Structured diagnostic and no partial replacement manifest. |

This contract intentionally leaves Maven query semantics, runtime application
models, and a universal build-system AST out of scope.
