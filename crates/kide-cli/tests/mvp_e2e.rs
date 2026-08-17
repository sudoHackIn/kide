use std::{ffi::OsStr, fs, path::Path, process::Command, time::Instant};

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

    let started = Instant::now();
    let cold = run_json(
        &workspace,
        ["index", workspace.to_str().expect("workspace path")],
    );
    assert_eq!(cold["status"], "ok");
    assert_eq!(cold["analyzed"], 7);
    assert!(cold["worker_starts"].as_u64().unwrap_or_default() >= 1);
    eprintln!(
        "spring_crud cold_index_ms={}",
        started.elapsed().as_millis()
    );

    let status = run_json(
        &workspace,
        ["--workspace", workspace.to_str().unwrap(), "status"],
    );
    assert_eq!(status["status"], "ok");
    assert_eq!(status["result"]["source_units"]["fresh"], 7);
    assert!(status["result"]["workers_running"]
        .as_array()
        .expect("workers array")
        .is_empty());

    let warm_query_started = Instant::now();
    let book_entity = run_json(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "symbols",
            "BookEntity",
        ],
    );
    let symbol = book_entity["result"]["symbols"][0].clone();
    eprintln!(
        "spring_crud warm_symbols_ms={}",
        warm_query_started.elapsed().as_millis()
    );
    assert_eq!(symbol["name"], "BookEntity");
    let book_entity_id = symbol["id"].as_str().expect("stable symbol id").to_owned();

    let definition = run_json(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "definition",
            &book_entity_id,
        ],
    );
    assert_eq!(definition["status"], "ok");
    assert_eq!(definition["result"]["symbol"]["id"], book_entity_id);

    let references = run_json(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "refs",
            &book_entity_id,
        ],
    );
    assert_eq!(references["status"], "ok");
    assert!(
        references["result"]["references"]
            .as_array()
            .expect("references")
            .len()
            >= 5
    );

    let implementations = run_json(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "implementations",
            &book_entity_id,
        ],
    );
    assert_eq!(implementations["status"], "no_result");

    let create = run_json(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "symbols",
            "create",
        ],
    );
    let create_id = create["result"]["symbols"][0]["id"]
        .as_str()
        .expect("create id");
    let callers = run_json(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "callers",
            create_id,
        ],
    );
    assert_eq!(callers["status"], "no_result");

    let type_at = run_json(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "type-at",
            "app/src/main/kotlin/dev/kide/fixture/book/BookController.kt:32:65",
        ],
    );
    assert_eq!(type_at["status"], "ok");
    assert_eq!(
        type_at["result"]["ty"]["display"],
        "dev/kide/fixture/book/BookEntity"
    );

    let entity_annotation = symbol["applied_symbols"]
        .as_array()
        .expect("applied symbols")
        .iter()
        .map(|value| value.as_str().expect("symbol id"))
        .find(|id| id.ends_with(":jakarta.persistence.Entity"))
        .expect("Entity annotation");
    let selected = run_json(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "select",
            "--kotlin-class",
            "--applies",
            entity_annotation,
        ],
    );
    assert_eq!(selected["symbol"]["id"], book_entity_id);

    let text = run_json_lines(
        &workspace,
        [
            "--workspace",
            workspace.to_str().unwrap(),
            "text",
            "BookEntity",
        ],
    );
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
    let stale = run_json(
        &workspace,
        ["--workspace", workspace.to_str().unwrap(), "status"],
    );
    assert_eq!(stale["result"]["source_units"]["stale"], 1);

    let incremental = run_json(&workspace, ["index", workspace.to_str().unwrap()]);
    assert_eq!(incremental["analyzed"], 1);
    assert_eq!(incremental["reused"], 6);
}

fn fixture_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/spring-boot-crud")
}

fn copy_fixture(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("creates fixture root");
    for entry in fs::read_dir(source).expect("reads fixture directory") {
        let entry = entry.expect("directory entry");
        let name = entry.file_name();
        if matches!(name.as_os_str(), value if value == OsStr::new(".gradle") || value == OsStr::new(".kide") || value == OsStr::new(".kotlin") || value == OsStr::new("build"))
        {
            continue;
        }
        let target = destination.join(&name);
        let kind = entry.file_type().expect("file type");
        if kind.is_dir() {
            copy_fixture(&entry.path(), &target);
        } else if kind.is_file() {
            fs::copy(entry.path(), target).expect("copies fixture file");
        }
    }
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

fn run_json_lines<const N: usize>(workspace: &Path, args: [&str; N]) -> Vec<Value> {
    let output = run(workspace, args);
    String::from_utf8(output.stdout)
        .expect("UTF-8 output")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSONL record"))
        .collect()
}

fn run<const N: usize>(workspace: &Path, args: [&str; N]) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_kide"))
        .args(args)
        .env(
            "KIDE_ARTIFACT_CACHE_DIR",
            workspace.join(".kide/artifact-cache"),
        )
        .env("GRADLE_HOME", gradle_installation())
        .output()
        .expect("runs kide");
    assert!(
        output.status.success() || !output.stdout.is_empty(),
        "kide failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
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
