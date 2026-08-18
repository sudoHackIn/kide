use std::{
    ffi::OsStr,
    fs,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;
use tempfile::tempdir;

/// Runs the complete agent-facing CLI contract against the realistic
/// multi-module Spring fixture. `make e2e` builds the disposable worker first.
#[test]
#[ignore = "requires the Kotlin worker distribution; run `make e2e`"]
fn spring_crud_mvp_survives_cold_restarts_and_incremental_updates() {
    let directory = tempdir().expect("temporary workspace");
    let workspace = directory.path().join("spring-boot-crud");
    copy_fixture(&fixture_root(), &workspace);
    write_query(
        &workspace,
        "spring.repositories.kql",
        "command spring.repositories() {\n  from subtype_of(qualified-symbol(\"org.springframework.data.jpa.repository.JpaRepository\"))\n  where kind == interface\n  return symbol\n  limit 100\n}\n",
    );

    let cold = measure("cold_index", || {
        run_json(
            &workspace,
            ["index", workspace.to_str().expect("workspace path")],
        )
    });
    assert_eq!(cold["status"], "ok", "cold index result: {cold}");
    assert_eq!(cold["analyzed"], 9);
    assert!(cold["worker_starts"].as_u64().unwrap_or_default() >= 1);

    let unchanged = measure("unchanged_index", || {
        run_json(&workspace, ["index", workspace.to_str().unwrap()])
    });
    assert_eq!(unchanged["analyzed"], 0);
    assert_eq!(unchanged["reused"], 9);
    assert_eq!(unchanged["worker_starts"], 0);

    let status = measure("warm_status", || {
        run_json(
            &workspace,
            ["--workspace", workspace.to_str().unwrap(), "status"],
        )
    });
    assert_eq!(status["status"], "ok");
    assert_eq!(status["result"]["source_units"]["fresh"], 9);
    assert!(status["result"]["workers_running"]
        .as_array()
        .expect("workers array")
        .is_empty());

    let book_entity = measure("warm_symbols", || {
        run_json(
            &workspace,
            [
                "--workspace",
                workspace.to_str().unwrap(),
                "symbols",
                "BookEntity",
            ],
        )
    });
    let symbol = book_entity["result"]["symbols"][0].clone();
    assert_eq!(symbol["name"], "BookEntity");
    let book_entity_id = symbol["id"].as_str().expect("stable symbol id").to_owned();

    let definition = measure("warm_definition", || {
        run_json(
            &workspace,
            [
                "--workspace",
                workspace.to_str().unwrap(),
                "definition",
                &book_entity_id,
            ],
        )
    });
    assert_eq!(definition["status"], "ok");
    assert_eq!(definition["result"]["symbol"]["id"], book_entity_id);

    let references = measure("warm_refs", || {
        run_json(
            &workspace,
            [
                "--workspace",
                workspace.to_str().unwrap(),
                "refs",
                &book_entity_id,
            ],
        )
    });
    assert_eq!(references["status"], "ok");
    assert!(
        references["result"]["references"]
            .as_array()
            .expect("references")
            .len()
            >= 5
    );

    let implementations = measure("warm_implementations", || {
        run_json(
            &workspace,
            [
                "--workspace",
                workspace.to_str().unwrap(),
                "implementations",
                &book_entity_id,
            ],
        )
    });
    assert_eq!(implementations["status"], "no_result");

    let create = measure("warm_create_symbols", || {
        run_json(
            &workspace,
            [
                "--workspace",
                workspace.to_str().unwrap(),
                "symbols",
                "create",
            ],
        )
    });
    let create_id = create["result"]["symbols"][0]["id"]
        .as_str()
        .expect("create id");
    let callers = measure("warm_callers", || {
        run_json(
            &workspace,
            [
                "--workspace",
                workspace.to_str().unwrap(),
                "callers",
                create_id,
            ],
        )
    });
    assert_eq!(callers["status"], "no_result");

    let type_at = measure("warm_type_at", || {
        run_json(
            &workspace,
            [
                "--workspace",
                workspace.to_str().unwrap(),
                "type-at",
                "app/src/main/kotlin/dev/kide/fixture/book/BookController.kt:32:65",
            ],
        )
    });
    assert_eq!(type_at["status"], "ok");
    assert_eq!(
        type_at["result"]["ty"]["display"],
        "dev/kide/fixture/book/BookEntity"
    );

    let java_audit = measure("warm_java_symbols", || {
        run_json(
            &workspace,
            ["--workspace", workspace.to_str().unwrap(), "symbols", "JavaBookAudit"],
        )
    });
    let java_audit_id = java_audit["result"]["symbols"][0]["id"].as_str().expect("JavaBookAudit id");
    assert_eq!(run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "refs", java_audit_id])["status"], "ok");
    assert_eq!(run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "implementations", java_audit_id])["status"], "ok");
    let audited = run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "symbols", "Audited"]);
    let audited_id = audited["result"]["symbols"][0]["id"].as_str().expect("Audited id");
    let java_selected = run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "select", "--java-class", "--applies", audited_id]);
    assert_eq!(java_selected["symbol"]["name"], "JavaBookAuditController");
    let controllers = run_json_lines(&workspace, ["--workspace", workspace.to_str().unwrap(), "query", "spring.controllers"]);
    let controller_names = controllers.iter().map(|record| record["symbol"]["name"].as_str().expect("symbol name")).collect::<Vec<_>>();
    assert!(controller_names.contains(&"BookController"));
    assert!(controller_names.contains(&"JavaBookAuditController"));
    let entities = run_json_lines(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "query",
            "persistence.entities",
        ],
    );
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0]["symbol"]["name"], "BookEntity");
    assert_eq!(
        entities[0]["plan"]["packages"][0]["id"],
        "jakarta.persistence"
    );
    assert_eq!(entities[0]["state"], "partial");
    let repositories = run_json_lines(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "query",
            "spring.repositories",
        ],
    );
    assert!(repositories.iter().any(|record| {
        record["symbol"]["qualified_name"] == "dev.kide.fixture.book.BookRepository"
    }));
    for (command, expected_name) in [
        ("spring.core.components", "IndexedComponent"),
        ("spring.core.services", "IndexedService"),
        ("spring.core.repositories", "IndexedRepository"),
        ("spring.core.configurations", "IndexedConfiguration"),
        ("spring.core.beans", "indexedBean"),
        ("spring.core.qualifiers", "QualifiedComponent"),
        ("spring.core.primaries", "indexedBean"),
        ("spring.boot.autoconfigurations", "IndexedAutoConfiguration"),
    ] {
        let records = run_json_lines(
            &workspace,
            ["--workspace", workspace.to_str().unwrap(), "query", command],
        );
        assert!(records.iter().any(|record| record["symbol"]["name"] == expected_name),
            "{command} should select {expected_name}");
        let package = if command == "spring.boot.autoconfigurations" {
            "spring.boot"
        } else {
            "spring.core"
        };
        assert_eq!(records[0]["plan"]["packages"][0]["id"], package);
    }
    let java_record = run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "symbols", "record"]);
    let java_record_id = java_record["result"]["symbols"].as_array().expect("record symbols").iter()
        .find(|symbol| symbol["qualified_name"] == "dev.kide.fixture.book.JavaBookAudit.record")
        .and_then(|symbol| symbol["id"].as_str()).expect("JavaBookAudit.record id");
    assert_eq!(run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "callers", java_record_id])["status"], "ok");
    assert_eq!(run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "type-at", "app/src/main/java/dev/kide/fixture/book/JavaBookAudit.java:22:19"])["status"], "ok");

    let entity_annotation = symbol["applied_symbols"]
        .as_array()
        .expect("applied symbols")
        .iter()
        .map(|value| value.as_str().expect("symbol id"))
        .find(|id| id.ends_with(":jakarta.persistence.Entity"))
        .expect("Entity annotation");
    let selected = measure("warm_select", || {
        run_json(
            &workspace,
            [
                "--workspace",
                workspace.to_str().unwrap(),
                "select",
                "--kotlin-class",
                "--applies",
                entity_annotation,
            ],
        )
    });
    assert_eq!(selected["symbol"]["id"], book_entity_id);

    let text = measure("warm_text", || {
        run_json_lines(
            &workspace,
            [
                "--workspace",
                workspace.to_str().unwrap(),
                "text",
                "BookEntity",
            ],
        )
    });
    assert!(text
        .iter()
        .any(|record| record["path"] == "app/src/main/kotlin/dev/kide/fixture/book/BookEntity.kt"));

    let entity_file = workspace.join("app/src/main/kotlin/dev/kide/fixture/book/BookEntity.kt");
    fs::write(
        &entity_file,
        format!(
            "{}\n// e2e body-only edit\n",
            fs::read_to_string(&entity_file).expect("fixture source")
        ),
    )
    .expect("edits source");
    let stale = measure("stale_status", || {
        run_json(
            &workspace,
            ["--workspace", workspace.to_str().unwrap(), "status"],
        )
    });
    assert_eq!(stale["result"]["source_units"]["stale"], 1);

    let incremental = measure("incremental_index", || {
        run_json(&workspace, ["index", workspace.to_str().unwrap()])
    });
    assert_eq!(
        incremental["status"],
        "ok",
        "incremental index result: {incremental}"
    );
    assert_eq!(incremental["analyzed"], 1);
    assert_eq!(incremental["reused"], 8);
}

/// Java source indexing exercises the same persisted navigation contract as
/// Kotlin while keeping the javac session and its memory disposable.
#[test]
#[ignore = "requires the JVM worker distribution; run `make e2e`"]
fn java_semantic_mvp_survives_cold_restarts_and_incremental_updates() {
    let directory = tempdir().expect("temporary workspace");
    let workspace = directory.path().join("java-semantic");
    copy_fixture(&java_fixture_root(), &workspace);
    write_query(
        &workspace,
        "api.implementations.kql",
        "command api.implementations(api: symbol-id) {\n  from implements($api)\n  where kind == class\n  where language == java\n  return symbol\n  limit 100\n}\n",
    );

    let (cold, cold_ms, peak_rss_kib) = run_json_measured(
        &workspace,
        ["index", workspace.to_str().expect("workspace path")],
    );
    eprintln!("java_semantic cold_index_ms={cold_ms} peak_rss_kib={peak_rss_kib}");
    assert_eq!(cold["status"], "ok");
    assert_eq!(cold["analyzed"], 3);
    assert!(cold["worker_starts"].as_u64().unwrap_or_default() >= 1);
    assert!(peak_rss_kib > 0, "could not observe CLI RSS while indexing");

    let status = run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "status"]);
    assert_eq!(status["result"]["source_units"]["fresh"], 3);
    assert!(status["result"]["workers_running"].as_array().expect("workers array").is_empty());

    let api = run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "symbols", "Api"]);
    let api_symbol = api["result"]["symbols"][0].clone();
    let api_id = api_symbol["id"].as_str().expect("Api id");
    assert_eq!(run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "definition", api_id])["status"], "ok");
    assert_eq!(run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "refs", api_id])["status"], "ok");
    assert_eq!(run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "implementations", api_id])["status"], "ok");
    let implementations = run_json_lines(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "query",
            "api.implementations",
            "--param",
            &format!("api={api_id}"),
        ],
    );
    assert!(implementations.iter().any(|record| {
        record["symbol"]["qualified_name"] == "fixture.Impl"
    }));

    let name = run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "symbols", "name"]);
    let api_name_id = name["result"]["symbols"].as_array().expect("name symbols").iter()
        .find(|symbol| symbol["qualified_name"] == "fixture.Api.name")
        .and_then(|symbol| symbol["id"].as_str())
        .expect("Api.name id");
    assert_eq!(run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "callers", api_name_id])["status"], "ok");
    let type_at = run_json(&workspace, ["--workspace", workspace.to_str().unwrap(), "type-at", "src/main/java/fixture/Use.java:4:18"]);
    assert_eq!(type_at["status"], "ok");
    assert_eq!(type_at["result"]["ty"]["display"], "fixture.Api");

    let use_file = workspace.join("src/main/java/fixture/Use.java");
    fs::write(&use_file, format!("{}\n// e2e body-only edit\n", fs::read_to_string(&use_file).expect("fixture source"))).expect("edits source");
    let incremental = run_json(&workspace, ["index", workspace.to_str().unwrap()]);
    assert_eq!(incremental["analyzed"], 1);
    assert_eq!(incremental["reused"], 2);
}

fn measure<T>(name: &str, operation: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = operation();
    eprintln!("spring_crud {name}_ms={}", started.elapsed().as_millis());
    result
}

fn fixture_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/spring-boot-crud")
}

fn java_fixture_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/java-semantic")
}

fn copy_fixture(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("creates fixture root");
    for entry in fs::read_dir(source).expect("reads fixture directory") {
        let entry = entry.expect("directory entry");
        let name = entry.file_name();
        if matches!(name.as_os_str(), value if value == OsStr::new(".gradle") || value == OsStr::new(".kotlin") || value == OsStr::new("build"))
        {
            continue;
        }
        let target = destination.join(&name);
        let kind = entry.file_type().expect("file type");
        if name == OsStr::new(".kide") {
            // Query definitions are fixture source; persistent cache/index
            // state is not, otherwise the cold-index assertion is invalid.
            for child in ["queries", "query-packages"] {
                let source_child = entry.path().join(child);
                if source_child.exists() {
                    copy_fixture(&source_child, &target.join(child));
                }
            }
            continue;
        }
        if kind.is_dir() {
            copy_fixture(&entry.path(), &target);
        } else if kind.is_file() {
            fs::copy(entry.path(), target).expect("copies fixture file");
        }
    }
}

fn write_query(workspace: &Path, name: &str, contents: &str) {
    let directory = workspace.join(".kide/queries");
    fs::create_dir_all(&directory).expect("creates fixture query directory");
    fs::write(directory.join(name), contents).expect("writes fixture query");
}

fn run_json<const N: usize>(workspace: &Path, args: [&str; N]) -> Value {
    let output = run(workspace, args);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "CLI did not produce JSON: {error}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn run_json_measured<const N: usize>(workspace: &Path, args: [&str; N]) -> (Value, u128, u64) {
    let started = Instant::now();
    let (output, peak_rss_kib) = run_measured(workspace, args);
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| panic!("CLI did not produce JSON: {error}; stderr={}", String::from_utf8_lossy(&output.stderr)));
    (value, started.elapsed().as_millis(), peak_rss_kib)
}

fn run_json_lines<const N: usize>(workspace: &Path, args: [&str; N]) -> Vec<Value> {
    let output = run(workspace, args);
    String::from_utf8(output.stdout)
        .expect("UTF-8 output")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSONL record"))
        .collect()
}

fn run<const N: usize>(workspace: &Path, args: [&str; N]) -> std::process::Output {
    let output = command(workspace, args)
        .output()
        .expect("runs kide");
    assert!(
        output.status.success() || !output.stdout.is_empty(),
        "kide failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn run_measured<const N: usize>(workspace: &Path, args: [&str; N]) -> (std::process::Output, u64) {
    let mut child = command(workspace, args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("runs kide");
    let mut peak_rss_kib = 0;
    while child.try_wait().expect("polls kide").is_none() {
        peak_rss_kib = peak_rss_kib.max(process_tree_rss_kib(child.id()));
        thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("collects kide output");
    assert!(output.status.success() || !output.stdout.is_empty(), "kide failed: {}", String::from_utf8_lossy(&output.stderr));
    (output, peak_rss_kib)
}

fn command<const N: usize>(workspace: &Path, args: [&str; N]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kide"));
    command
        .args(args)
        .env(
            "KIDE_ARTIFACT_CACHE_DIR",
            workspace.join(".kide/artifact-cache"),
        )
        .env("GRADLE_HOME", gradle_installation());
    command
}

fn process_tree_rss_kib(pid: u32) -> u64 {
    let mut pids = vec![pid];
    let children = Command::new("pgrep").args(["-P", &pid.to_string()]).output().ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.lines().filter_map(|line| line.parse::<u32>().ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    pids.extend(children);
    pids.into_iter().map(process_rss_kib).sum()
}

fn process_rss_kib(pid: u32) -> u64 {
    Command::new("ps").args(["-o", "rss=", "-p", &pid.to_string()]).output().ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.trim().parse().ok()).unwrap_or_default()
}

fn gradle_installation() -> std::path::PathBuf {
    std::env::var_os("KIDE_GRADLE_INSTALLATION")
        .or_else(|| std::env::var_os("GRADLE_HOME"))
        .map(std::path::PathBuf::from)
        .filter(|path| path.join("bin/gradle").is_file())
        .or_else(|| {
            let root = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)?
                .join(".gradle/wrapper/dists");
            find_gradle_installation(&root, 4)
        })
        .expect("an installed Gradle distribution; run `make build` first")
}

fn find_gradle_installation(root: &Path, depth: usize) -> Option<std::path::PathBuf> {
    if root.join("bin/gradle").is_file() {
        return Some(root.to_owned());
    }
    (depth != 0).then_some(())?;
    fs::read_dir(root).ok()?.flatten().find_map(|entry| {
        entry
            .file_type()
            .ok()?
            .is_dir()
            .then(|| find_gradle_installation(&entry.path(), depth - 1))?
    })
}
