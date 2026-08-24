# KIDE

KIDE is a local semantic code index. Core owns durable canonical facts; disposable language workers resolve build models and produce semantic snapshots.

## Quick start

Build the CLI and JVM worker:

```bash
make build
```

Initialize the repository once. The generated config is intended to be committed; `.kide/.gitignore` keeps the index and worker state local.

```bash
kide --workspace /path/to/project init
kide index /path/to/project
kide --workspace /path/to/project status
```

`status` is worker-free. It verifies the persisted checkpoint against current sources and configuration and reports `fresh`, `stale`, or `unknown`.

Preview incremental work without starting Maven, Gradle, or a language worker:

```bash
kide index --plan /path/to/project
```

The stable JSON result reports whether the resolved manifest can be reused,
source `reused`/`analyze`/`removed` counts, dependency cache hits and misses,
and cached blob bytes. When build inputs changed it reports
`manifest: "resolve_required"`; run `kide index` to obtain the authoritative
new classpath.

## Maven projects with unavailable private dependencies

For repositories whose Maven dependencies are only available on a private repository, keep indexing source facts while recording unavailable dependencies as unresolved:

```toml
# .kide/config.toml
[maven]
best_effort_dependencies = true # default; set false to fail fast
```

This is equivalent to the worker's legacy `KIDE_MAVEN_BEST_EFFORT=1` environment setting, but is tracked with the workspace configuration.

## Timing and interactive traces

Print nested timing spans for a command:

```bash
RUST_LOG=kide=debug \
  kide --workspace /path/to/project status
```

Export a timeline trace for Perfetto or `chrome://tracing`:

```bash
KIDE_TRACE_CHROME=/tmp/kide-status.json \
RUST_LOG=kide=debug \
  kide --workspace /path/to/project status
```

Export a drill-down flamegraph trace for [Speedscope](https://www.speedscope.app):

```bash
KIDE_TRACE_FLAME=/tmp/kide-status.folded \
RUST_LOG=kide=debug \
  kide --workspace /path/to/project status
```

`KIDE_TRACE_FLAME` takes precedence when both trace variables are set. The trace files are local diagnostics and should not be committed.

For a 1,200-source workspace, the detailed status spans make the dominant work explicit: filesystem discovery and source fingerprint validation, rather than SQLite or worker startup.
