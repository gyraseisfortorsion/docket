//! Opt-in real-engine/Python integration suite. Run with --include-ignored after
//! `contracts engine install` and setting CONTRACTS_TEST_PYTHON to a FastAPI environment.
use assert_cmd::Command;
use contracts::{compare, engine, report, store, util};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command as Process,
    time::Duration,
};
use tempfile::TempDir;

fn cli(root: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("contracts"));
    command.current_dir(root);
    command
}
fn example() -> Value {
    serde_json::from_str(include_str!("../examples/users.openapi.json")).unwrap()
}
fn python() -> PathBuf {
    std::env::var_os("CONTRACTS_TEST_PYTHON")
        .map(PathBuf::from)
        .expect("set CONTRACTS_TEST_PYTHON to an interpreter with FastAPI installed")
}
fn project() -> TempDir {
    project_with_history("full")
}
fn project_with_history(mode: &str) -> TempDir {
    engine::locate().expect("run cargo run -- engine install");
    let directory = tempfile::tempdir().unwrap();
    util::write_json(&directory.path().join("openapi.json"), &example()).unwrap();
    cli(directory.path())
        .args([
            "init",
            "--service",
            "users-api",
            "--file",
            "openapi.json",
            "--history",
            mode,
        ])
        .assert()
        .success();
    cli(directory.path()).arg("sync").assert().success();
    directory
}
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Process::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn git_init(root: &Path) {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Contracts Tests"]);
    git(root, &["config", "user.email", "contracts@example.invalid"]);
    git(root, &["config", "commit.gpgsign", "false"]);
}
fn initial_commit(root: &Path) {
    git_init(root);
    git(root, &["add", "."]);
    git(root, &["commit", "--no-verify", "-qm", "initial"]);
}
fn compare_documents(old: Value, new: Value) -> compare::Comparison {
    compare::compare(
        &BTreeMap::from([("users".into(), old)]),
        &BTreeMap::from([("users".into(), new)]),
        Duration::from_secs(30),
    )
    .unwrap()
}

#[test]
#[ignore = "requires pinned oasdiff"]
fn compatibility_is_direction_sensitive_for_requests_responses_auth_enums_and_nullability() {
    let old = example();
    let mut required = old.clone();
    required["components"]["schemas"]["UserCreate"]["properties"]["email"] =
        json!({"type":"string"});
    required["components"]["schemas"]["UserCreate"]["required"] = json!(["name", "email"]);
    assert!(
        compare_documents(old.clone(), required).breaking() > 0,
        "required request field"
    );

    let mut removed = old.clone();
    removed["components"]["schemas"]["UserResponse"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("name");
    removed["components"]["schemas"]["UserResponse"]["required"] = json!(["id"]);
    assert!(
        compare_documents(old.clone(), removed).breaking() > 0,
        "removed response field"
    );

    let mut auth = old.clone();
    auth["components"]["securitySchemes"] = json!({"bearer":{"type":"http","scheme":"bearer"}});
    auth["security"] = json!([{"bearer":[]}]);
    assert!(
        compare_documents(old.clone(), auth).breaking() > 0,
        "new auth requirement"
    );

    let mut nullable = old.clone();
    nullable["components"]["schemas"]["UserResponse"]["properties"]["name"]["type"] =
        json!(["string", "null"]);
    assert!(
        compare_documents(old.clone(), nullable).breaking() > 0,
        "nullable response"
    );
    let mut nullable_request = old.clone();
    nullable_request["components"]["schemas"]["UserCreate"]["properties"]["name"]["type"] =
        json!(["string", "null"]);
    assert_eq!(
        compare_documents(old.clone(), nullable_request).breaking(),
        0,
        "request widening"
    );

    let mut enum_base = old.clone();
    enum_base["components"]["schemas"]["UserCreate"]["properties"]["name"]["enum"] =
        json!(["Ada", "Grace"]);
    let mut narrowed = enum_base.clone();
    narrowed["components"]["schemas"]["UserCreate"]["properties"]["name"]["enum"] = json!(["Ada"]);
    assert!(
        compare_documents(enum_base.clone(), narrowed).breaking() > 0,
        "request enum narrowing"
    );
    assert_eq!(
        compare_documents(enum_base, old).breaking(),
        0,
        "request enum widening"
    );
}

#[test]
#[ignore = "requires pinned oasdiff"]
fn all_renderers_share_complete_changes_inventory_and_safe_html() {
    let mut new = example();
    new["paths"]["/users"]["post"]["description"] = json!("<script>alert('unsafe')</script> ``` ");
    let old = BTreeMap::from([("users".into(), example())]);
    let new = BTreeMap::from([("users".into(), new)]);
    let comparison = compare::compare(&old, &new, Duration::from_secs(30)).unwrap();
    assert_eq!(comparison.breaking(), 0);
    assert!(!comparison.changes.is_empty());
    assert_eq!(comparison.changes[0].kind, "documentation");
    let result = report::build("before", &old, &new, comparison, Vec::new()).unwrap();
    for format in [
        report::Format::Json,
        report::Format::Markdown,
        report::Format::Txt,
        report::Format::Html,
    ] {
        let rendered = report::render(&result, format).unwrap();
        assert!(rendered.contains("UserCreate"));
        assert!(rendered.contains("/paths/~1users/post/description"));
        if matches!(format, report::Format::Html) {
            assert!(!rendered.contains("<script>"));
            assert!(rendered.contains("&lt;script&gt;"));
        }
    }
    let json: Value =
        serde_json::from_str(&report::render(&result, report::Format::Json).unwrap()).unwrap();
    assert_eq!(json["comparison"]["changes"].as_array().unwrap().len(), 1);
    assert_eq!(
        json["comparison"]["compatibility_engine"],
        format!("oasdiff {}", engine::VERSION)
    );
}

#[test]
#[ignore = "requires pinned oasdiff"]
fn git_hook_records_staged_spec_and_preserves_unstaged_source() {
    let project = project();
    let root = project.path();
    initial_commit(root);
    cli(root).args(["hooks", "install"]).assert().success();
    let mut staged = example();
    staged["components"]["schemas"]["UserCreate"]["properties"]["email"] = json!({"type":"string"});
    staged["components"]["schemas"]["UserCreate"]["required"] = json!(["name", "email"]);
    util::write_json(&root.join("openapi.json"), &staged).unwrap();
    git(root, &["add", "openapi.json"]);
    let mut unstaged = staged.clone();
    unstaged["info"]["description"] = json!("not staged");
    util::write_json(&root.join("openapi.json"), &unstaged).unwrap();
    let output = Process::new("git")
        .current_dir(root)
        .args(["commit", "-qm", "change"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("WARNING"));
    let committed: Value = serde_json::from_slice(&git(
        root,
        &["show", "HEAD:.contracts/head/users-api/openapi.json"],
    ))
    .unwrap();
    assert_eq!(committed, staged);
    assert_eq!(
        util::read_json::<Value>(&root.join("openapi.json")).unwrap(),
        unstaged
    );
    assert_eq!(
        String::from_utf8_lossy(&git(root, &["diff", "--name-only"])).trim(),
        "openapi.json"
    );
    assert!(git(root, &["diff", "--cached", "--name-only"]).is_empty());
    assert_eq!(store::revisions(root).unwrap().len(), 2);
}

#[test]
#[ignore = "requires pinned oasdiff"]
fn git_head_only_baselines_and_ci_reject_stale_snapshots() {
    let project = project_with_history("head");
    let root = project.path();
    assert!(!root.join(".contracts/history").exists());
    initial_commit(root);
    cli(root)
        .args(["check", "--against", "git:HEAD"])
        .assert()
        .success();
    let mut changed = example();
    changed["info"]["description"] = json!("docs");
    util::write_json(&root.join("openapi.json"), &changed).unwrap();
    cli(root)
        .args(["check", "--against", "git:HEAD"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("stale head: true"));
    cli(root).arg("sync").assert().success();
    cli(root)
        .args(["check", "--against", "git:HEAD"])
        .assert()
        .success();
    cli(root)
        .args([
            "diff", "--from", "git:HEAD", "--to", "head", "--format", "json",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("documentation"));
}

#[test]
#[ignore = "requires pinned oasdiff and FastAPI Python"]
fn fastapi_partial_commit_prefers_staged_src_over_editable_source() {
    engine::locate().unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let app = "from fastapi import FastAPI\nfrom pydantic import BaseModel\napp=FastAPI()\nclass Body(BaseModel):\n    name: str\n@app.post('/users')\ndef create_user(body: Body):\n    return body\n";
    util::write_atomic(&root.join("src/users/application.py"), app.as_bytes()).unwrap();
    let editable = tempfile::tempdir().unwrap();
    // sitecustomize simulates the source-path injection performed by editable installs.
    util::write_atomic(
        &editable.path().join("sitecustomize.py"),
        format!(
            "import sys\nsys.path.insert(0, {})\n",
            serde_json::to_string(&root.join("src").to_string_lossy()).unwrap()
        )
        .as_bytes(),
    )
    .unwrap();
    cli(root)
        .args([
            "init",
            "--service",
            "users-api",
            "--fastapi",
            "users.application:app",
            "--python",
        ])
        .arg(python())
        .args(["--import-path", "src"])
        .assert()
        .success();
    cli(root)
        .arg("sync")
        .env("PYTHONPATH", editable.path())
        .assert()
        .success();
    initial_commit(root);
    cli(root).args(["hooks", "install"]).assert().success();
    let staged = app.replace("    name: str", "    name: str\n    email: str");
    util::write_atomic(&root.join("src/users/application.py"), staged.as_bytes()).unwrap();
    git(root, &["add", "src/users/application.py"]);
    let unstaged = staged.replace("    email: str", "    email: str\n    age: int");
    util::write_atomic(&root.join("src/users/application.py"), unstaged.as_bytes()).unwrap();
    let output = Process::new("git")
        .current_dir(root)
        .env("PYTHONPATH", editable.path())
        .args(["commit", "-qm", "staged API"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: Value = serde_json::from_slice(&git(
        root,
        &["show", "HEAD:.contracts/head/users-api/openapi.json"],
    ))
    .unwrap();
    let required = &doc["components"]["schemas"]["Body"]["required"];
    assert_eq!(required, &json!(["name", "email"]));
    assert_eq!(
        fs::read_to_string(root.join("src/users/application.py")).unwrap(),
        unstaged
    );
}

#[test]
#[ignore = "requires pinned oasdiff and FastAPI Python"]
fn fastapi_rejects_editable_modules_outside_the_staged_export() {
    engine::locate().unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    util::write_atomic(
        &root.join("src/users/application.py"),
        b"from fastapi import FastAPI\napp=FastAPI()\n",
    )
    .unwrap();
    let editable = tempfile::tempdir().unwrap();
    util::write_atomic(
        &editable.path().join("sitecustomize.py"),
        format!(
            "import sys\nsys.path.insert(0, {})\n",
            serde_json::to_string(&root.join("src").to_string_lossy()).unwrap()
        )
        .as_bytes(),
    )
    .unwrap();
    cli(root)
        .args([
            "init",
            "--service",
            "users-api",
            "--fastapi",
            "users.application:app",
            "--python",
        ])
        .arg(python())
        .assert()
        .success();
    cli(root)
        .arg("sync")
        .env("PYTHONPATH", editable.path())
        .assert()
        .success();
    initial_commit(root);
    cli(root).args(["hooks", "install"]).assert().success();
    util::write_atomic(
        &root.join("src/users/application.py"),
        b"from fastapi import FastAPI\napp=FastAPI(title='Changed')\n",
    )
    .unwrap();
    git(root, &["add", "src/users/application.py"]);
    let before = fs::read(root.join(".contracts/head/manifest.json")).unwrap();
    let index = fs::read(root.join(".git/index")).unwrap();
    let output = Process::new("git")
        .current_dir(root)
        .env("PYTHONPATH", editable.path())
        .args(["commit", "-qm", "must fail"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("outside the staged export"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        before,
        fs::read(root.join(".contracts/head/manifest.json")).unwrap()
    );
    // Git itself may refresh stat/cache extensions, so compare staged tree identity.
    assert!(!index.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&git(root, &["diff", "--cached", "--name-only"])).trim(),
        "src/users/application.py"
    );
}

#[test]
#[ignore = "requires Python"]
fn export_commands_and_targeted_proposal_edits_are_validated() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    util::write_json(&root.join("openapi.json"), &example()).unwrap();
    util::write_atomic(
        &root.join("export.py"),
        b"import pathlib\nprint(pathlib.Path('openapi.json').read_text())\n",
    )
    .unwrap();
    let export = format!(
        "{} export.py",
        shell_words::quote(&python().to_string_lossy())
    );
    cli(root)
        .args(["init", "--service", "users-api", "--command", &export])
        .assert()
        .success();
    cli(root).arg("sync").assert().success();
    cli(root)
        .args(["propose", "edit-schema"])
        .assert()
        .success();
    let editor = root.join("editor.py");
    util::write_atomic(&editor,b"import json,sys,pathlib\np=pathlib.Path(sys.argv[1]); d=json.loads(p.read_text()); d['properties']['preference']={'type':'string'}; p.write_text(json.dumps(d))\n").unwrap();
    let editor_command = format!(
        "{} {}",
        shell_words::quote(&python().to_string_lossy()),
        shell_words::quote(&editor.to_string_lossy())
    );
    cli(root)
        .args(["edit", "edit-schema", "--schema", "UserCreate"])
        .env("EDITOR", &editor_command)
        .env_remove("VISUAL")
        .assert()
        .success();
    let path = root.join(".contracts/proposals/edit-schema/users-api/openapi.json");
    assert_eq!(
        util::read_json::<Value>(&path).unwrap()["components"]["schemas"]["UserCreate"]["properties"]
            ["preference"]["type"],
        "string"
    );
    let before = fs::read(&path).unwrap();
    util::write_atomic(
        &editor,
        b"import pathlib,sys\npathlib.Path(sys.argv[1]).write_text('invalid json')\n",
    )
    .unwrap();
    cli(root)
        .args(["edit", "edit-schema", "--schema", "UserCreate"])
        .env("EDITOR", &editor_command)
        .env_remove("VISUAL")
        .assert()
        .failure()
        .stderr(predicates::str::contains("recover your edit"));
    assert_eq!(before, fs::read(&path).unwrap());
    util::write_atomic(
        &root.join("export.py"),
        b"raise RuntimeError('export failed')\n",
    )
    .unwrap();
    let head = fs::read(root.join(".contracts/head/manifest.json")).unwrap();
    cli(root).arg("sync").assert().failure();
    assert_eq!(
        head,
        fs::read(root.join(".contracts/head/manifest.json")).unwrap()
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires pinned oasdiff"]
fn existing_hooks_and_core_hooks_path_are_preserved() {
    use std::os::unix::fs::PermissionsExt;
    let project = project();
    let root = project.path();
    initial_commit(root);
    git(root, &["config", "core.hooksPath", ".custom-hooks"]);
    let hook = root.join(".custom-hooks/pre-commit");
    let original = b"#!/bin/sh\nprintf 'ran' > original-hook-ran\n";
    util::write_atomic(&hook, original).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    cli(root).args(["hooks", "install"]).assert().success();
    util::write_atomic(&root.join("README.txt"), b"docs").unwrap();
    git(root, &["add", "README.txt"]);
    git(root, &["commit", "-qm", "docs only"]);
    assert_eq!(
        fs::read_to_string(root.join("original-hook-ran")).unwrap(),
        "ran"
    );
    cli(root).args(["hooks", "uninstall"]).assert().success();
    assert_eq!(fs::read(hook).unwrap(), original);
}

#[test]
#[ignore = "requires FastAPI Python"]
fn fastapi_extraction_ignores_stale_bytecode_after_same_size_edits() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    util::write_atomic(
        &root.join("app.py"),
        b"from fastapi import FastAPI\napp=FastAPI(title='Alpha')\n",
    )
    .unwrap();
    let script = "import app,os,pathlib\np=pathlib.Path('app.py'); s=p.stat(); p.write_text(p.read_text().replace('Alpha','Bravo')); os.utime(p,ns=(s.st_atime_ns,s.st_mtime_ns))";
    let result = Process::new(python())
        .current_dir(root)
        .args(["-c", script])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    cli(root)
        .args([
            "init",
            "--service",
            "users-api",
            "--fastapi",
            "app:app",
            "--python",
        ])
        .arg(python())
        .assert()
        .success();
    cli(root).arg("sync").assert().success();
    assert_eq!(
        store::head(root).unwrap()["users-api"]["info"]["title"],
        "Bravo"
    );
}

#[test]
#[ignore = "requires FastAPI Python"]
fn fastapi_factory_does_not_start_lifespan_or_pollute_json_with_app_prints() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    util::write_atomic(&root.join("app.py"),b"from fastapi import FastAPI\nfrom contextlib import asynccontextmanager\nprint('application diagnostics')\n@asynccontextmanager\nasync def lifespan(app):\n    raise RuntimeError('lifespan must not start')\n    yield\ndef create_app():\n    return FastAPI(title='Factory',lifespan=lifespan)\n").unwrap();
    cli(root)
        .args([
            "init",
            "--service",
            "users-api",
            "--fastapi",
            "app:create_app",
            "--factory",
            "--python",
        ])
        .arg(python())
        .assert()
        .success();
    let output = cli(root)
        .args(["sync", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let envelope: Value = serde_json::from_slice(&output).unwrap();
    assert!(
        envelope["timings"]["users-api"]["import_ms"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert_eq!(
        store::head(root).unwrap()["users-api"]["info"]["title"],
        "Factory"
    );
}
