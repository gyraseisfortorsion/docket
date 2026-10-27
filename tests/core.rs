use assert_cmd::Command;
use docket::{compare, config::Config, openapi, proposal, store, util};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};
use tempfile::TempDir;

#[test]
fn invalid_schema_structure_does_not_replace_a_valid_snapshot() {
    let project = project("head");
    let root = project.path();
    let before = fs::read(root.join(".docket/head/manifest.json")).unwrap();
    for schema in [
        json!({"type":"strnig"}),
        json!({"type":"object","required":"name"}),
        json!({"type":"object","properties":[]}),
        json!({"type":"object","properties":{"name":{"type":42}}}),
    ] {
        let mut document = example();
        document["components"]["schemas"]["UserCreate"] = schema;
        util::write_json(&root.join("openapi.json"), &document).unwrap();
        cli(root).arg("sync").assert().failure();
        assert_eq!(
            before,
            fs::read(root.join(".docket/head/manifest.json")).unwrap()
        );
    }
}

fn cli(root: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("docket"));
    command.current_dir(root);
    command
}
fn example() -> Value {
    serde_json::from_str(include_str!("../examples/users.openapi.json")).unwrap()
}
fn project(mode: &str) -> TempDir {
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
fn snapshot_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let base = root.join(".docket");
    util::files(&base)
        .unwrap()
        .into_iter()
        .filter(|p| !p.starts_with("local"))
        .map(|p| {
            (
                p.to_string_lossy().into_owned(),
                fs::read(base.join(&p)).unwrap(),
            )
        })
        .collect()
}

#[test]
fn cli_help_is_valid() {
    cli(Path::new(env!("CARGO_MANIFEST_DIR")))
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn full_round_trip_and_noop_preserves_all_bytes_and_mtime() {
    let project = project("full");
    let root = project.path();
    let before = snapshot_files(root);
    let mtime = fs::metadata(root.join(".docket/head/manifest.json"))
        .unwrap()
        .modified()
        .unwrap();
    fs::write(
        root.join("openapi.json"),
        serde_json::to_vec(&example()).unwrap(),
    )
    .unwrap();
    cli(root)
        .args(["sync", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"changed\": false"));
    assert_eq!(before, snapshot_files(root));
    assert_eq!(
        mtime,
        fs::metadata(root.join(".docket/head/manifest.json"))
            .unwrap()
            .modified()
            .unwrap()
    );
    assert_eq!(store::revisions(root).unwrap().len(), 1);
    let record = store::revisions(root).unwrap().remove(0);
    assert_eq!(
        store::load_revision(root, &record.id).unwrap(),
        store::head(root).unwrap()
    );
}

#[test]
fn head_only_creates_no_archive() {
    let project = project("head");
    assert!(!project.path().join(".docket/history").exists());
    assert!(!project.path().join(".docket/objects").exists());
    assert!(
        store::manifest(project.path())
            .unwrap()
            .unwrap()
            .revision
            .is_none()
    );
}

#[test]
fn history_is_portable_without_source_or_git() {
    let source = project("full");
    let target = tempfile::tempdir().unwrap();
    for (path, bytes) in snapshot_files(source.path()) {
        util::write_atomic(&target.path().join(".docket").join(path), &bytes).unwrap();
    }
    let revision = store::revisions(target.path()).unwrap().remove(0);
    assert_eq!(
        store::load_revision(target.path(), &revision.id).unwrap(),
        store::head(source.path()).unwrap()
    );
    cli(target.path())
        .args(["show", "--operation", "POST /users"])
        .assert()
        .success();
}

#[test]
fn immutable_revisions_have_parents_and_deduplicated_objects() {
    let project = project("full");
    let root = project.path();
    let first = store::revisions(root).unwrap().remove(0);
    let mut value = example();
    value["info"]["description"] = json!("new docs");
    util::write_json(&root.join("openapi.json"), &value).unwrap();
    cli(root).arg("sync").assert().success();
    let records = store::revisions(root).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].parent.as_ref(), Some(&first.id));
    util::write_json(&root.join("openapi.json"), &example()).unwrap();
    cli(root).arg("sync").assert().success();
    assert_eq!(store::revisions(root).unwrap().len(), 3);
    assert_eq!(util::files(&root.join(".docket/objects")).unwrap().len(), 2);
}

#[test]
fn modified_or_deleted_generated_views_are_not_overwritten() {
    let project = project("full");
    let root = project.path();
    let schema = root.join(".docket/head/users-api/schemas/UserCreate.json");
    fs::write(&schema, "manual changes").unwrap();
    cli(root)
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicates::str::contains("move edits into a proposal"));
    assert_eq!(fs::read_to_string(&schema).unwrap(), "manual changes");
}

#[test]
fn missing_reference_and_unsupported_version_preserve_head() {
    let project = project("full");
    let root = project.path();
    let before = snapshot_files(root);
    let mut document = example();
    document["components"]["schemas"]["UserCreate"] =
        json!({"$ref":"#/components/schemas/Missing"});
    util::write_json(&root.join("openapi.json"), &document).unwrap();
    cli(root)
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicates::str::contains("unresolved reference"));
    assert_eq!(before, snapshot_files(root));
    document = example();
    document["openapi"] = json!("3.2.0");
    util::write_json(&root.join("openapi.json"), &document).unwrap();
    cli(root)
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicates::str::contains("unsupported OpenAPI"));
    assert_eq!(before, snapshot_files(root));
}

#[test]
fn local_and_recursive_references_are_bundled_portably() {
    let root = tempfile::tempdir().unwrap();
    let mut doc = example();
    doc["components"]["schemas"]["UserCreate"] = json!({"$ref":"models.json#/$defs/Node"});
    util::write_json(&root.path().join("models.json"),&json!({"$defs":{"Node":{"type":"object","properties":{"next":{"$ref":"#/$defs/Node"},"value":{"type":"string"}}}}})).unwrap();
    let bundled = openapi::bundle(root.path(), &root.path().join("openapi.json"), doc).unwrap();
    openapi::validate_snapshot(&bundled).unwrap();
    let operations = openapi::operations(&bundled).unwrap();
    assert!(operations["POST /users"].referenced_definitions.len() >= 2);
    fs::remove_file(root.path().join("models.json")).unwrap();
    openapi::validate_snapshot(&bundled).unwrap();
}

#[test]
fn literal_examples_with_ref_keys_are_not_resolved() {
    let root = tempfile::tempdir().unwrap();
    let mut doc = example();
    doc["paths"]["/users"]["post"]["requestBody"]["content"]["application/json"]["example"] =
        json!({"$ref":"literal-data-not-a-schema"});
    assert!(openapi::bundle(root.path(), &root.path().join("openapi.json"), doc).is_ok());
}

#[test]
fn tags_and_duplicate_operation_ids_have_stable_collision_free_names() {
    let mut doc = example();
    doc["paths"]["/users/{id}"]["get"]["operationId"] = json!("createUser");
    doc["paths"]["/users/{id}"]["get"]["tags"] = json!(["Users"]);
    let views = openapi::views("users-api", &doc).unwrap();
    assert_eq!(
        views.keys().filter(|p| p.starts_with("operations")).count(),
        2
    );
    assert_eq!(views, openapi::views("users-api", &doc).unwrap());
    doc["paths"]["/users"]["post"]
        .as_object_mut()
        .unwrap()
        .remove("operationId");
    doc["paths"]["/users"]["post"]
        .as_object_mut()
        .unwrap()
        .remove("tags");
    assert_eq!(
        openapi::operations(&doc).unwrap()["POST /users"].tag,
        "users"
    );
}

#[test]
fn shared_model_changes_include_transitive_endpoint_impact() {
    let mut before = example();
    before["components"]["schemas"]["UserResponse"]["properties"]["address"] =
        json!({"$ref":"#/components/schemas/Address"});
    before["components"]["schemas"]["Address"] =
        json!({"type":"object","properties":{"city":{"type":"string"}}});
    let mut after = before.clone();
    after["components"]["schemas"]["Address"]["properties"]["city"]["type"] = json!("integer");
    let comparison = compare::structural(
        &BTreeMap::from([("users".into(), before)]),
        &BTreeMap::from([("users".into(), after)]),
    )
    .unwrap();
    assert_eq!(comparison.changes.len(), 1);
    assert_eq!(
        comparison.changes[0].affected_operations,
        vec!["GET /users/{id}", "POST /users"]
    );
}

#[test]
fn proposal_survives_sync_and_closes_explicitly() {
    let project = project("head");
    let root = project.path();
    cli(root)
        .args(["propose", "preferences"])
        .assert()
        .success();
    let path = root.join(".docket/proposals/preferences/users-api/openapi.json");
    let mut desired: Value = util::read_json(&path).unwrap();
    desired["info"]["description"] = json!("desired change");
    util::write_json(&path, &desired).unwrap();
    let bytes = fs::read(&path).unwrap();
    cli(root).arg("sync").assert().success();
    assert_eq!(bytes, fs::read(&path).unwrap());
    assert_eq!(
        proposal::drifts(root, &store::head(root).unwrap()).unwrap()[0]
            .changes
            .len(),
        1
    );
    cli(root).args(["close", "preferences"]).assert().success();
    assert!(
        proposal::drifts(root, &store::head(root).unwrap())
            .unwrap()
            .is_empty()
    );
    assert!(path.exists());
}

#[test]
fn multi_service_configuration_is_supported() {
    let project = project("head");
    cli(project.path())
        .args(["init", "--service", "payments", "--file", "openapi.json"])
        .assert()
        .success();
    cli(project.path()).arg("sync").assert().success();
    assert_eq!(Config::load(project.path()).unwrap().services.len(), 2);
    cli(project.path()).arg("show").assert().failure();
    cli(project.path())
        .args(["show", "--service", "payments"])
        .assert()
        .success();
}

#[test]
fn schema_named_description_is_a_structural_change() {
    let mut old = example();
    old["components"]["schemas"]["UserCreate"]["properties"]["description"] =
        json!({"type":"string"});
    let mut new = old.clone();
    new["components"]["schemas"]["UserCreate"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("description");
    let result = compare::structural(
        &BTreeMap::from([("users".into(), old)]),
        &BTreeMap::from([("users".into(), new)]),
    )
    .unwrap();
    assert_eq!(result.changes[0].kind, "structural");
}

#[test]
fn operation_changes_do_not_mark_other_methods_on_the_same_path() {
    let mut old = example();
    old["paths"]["/users"]["get"] = json!({"responses":{"200":{"description":"List users"}}});
    let mut new = old.clone();
    new["paths"]["/users"]["post"]["summary"] = json!("Create a new user");
    let comparison = compare::structural(
        &BTreeMap::from([("users".into(), old)]),
        &BTreeMap::from([("users".into(), new)]),
    )
    .unwrap();
    assert_eq!(
        comparison.changes[0].affected_operations,
        vec!["POST /users"]
    );
}

#[test]
fn referenced_path_items_and_parameters_keep_precise_source_impact() {
    let mut old = example();
    old["components"]["parameters"] =
        json!({"Trace":{"name":"trace","in":"header","schema":{"type":"string"}}});
    old["paths"]["/users/{id}"]["parameters"]
        .as_array_mut()
        .unwrap()
        .push(json!({"$ref":"#/components/parameters/Trace"}));
    old["components"]["pathItems"] = json!({"Users":old["paths"]["/users"].clone()});
    old["paths"]["/users"] = json!({"$ref":"#/components/pathItems/Users"});
    let mut new = old.clone();
    new["components"]["pathItems"]["Users"]["post"]["summary"] = json!("Changed summary");
    new["components"]["parameters"]["Trace"]["required"] = json!(true);
    let comparison = compare::structural(
        &BTreeMap::from([("users".into(), old)]),
        &BTreeMap::from([("users".into(), new)]),
    )
    .unwrap();
    for change in comparison.changes {
        if change.pointer.contains("pathItems") {
            assert_eq!(change.affected_operations, vec!["POST /users"]);
        } else {
            assert_eq!(change.affected_operations, vec!["GET /users/{id}"]);
        }
    }
}

#[test]
fn schema_filename_collisions_and_service_case_collisions_are_handled() {
    let mut doc = example();
    let hash = util::hash(b"user");
    let reserved = format!("user--{}-0", &hash[..16]);
    doc["paths"] = json!({});
    doc["components"]["schemas"] = json!({"User":{"type":"string"},"user":{"type":"integer"}});
    doc["components"]["schemas"][&reserved] = json!({"type":"boolean"});
    let files = openapi::views("users", &doc).unwrap();
    assert_eq!(files.keys().filter(|p| p.starts_with("schemas")).count(), 3);
    let project = project("head");
    cli(project.path())
        .args(["init", "--service", "Users-Api", "--file", "openapi.json"])
        .assert()
        .failure();
}

#[test]
fn extraction_timeout_preserves_the_previous_snapshot() {
    let project = project("head");
    let root = project.path();
    let before = snapshot_files(root);
    let mut config = Config::load(root).unwrap();
    config.timeout_seconds = 1;
    config.services.get_mut("users-api").unwrap().source = docket::config::Source::Command {
        command: vec!["sh".into(), "-c".into(), "sleep 15".into()],
        cwd: None,
    };
    config.save(root).unwrap();
    let start = std::time::Instant::now();
    cli(root)
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicates::str::contains("timed out"));
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(
        fs::read(root.join(".docket/head/manifest.json")).unwrap(),
        before["head/manifest.json"]
    );
}

#[test]
fn watch_debounces_input_changes_and_does_not_watch_its_own_output() {
    let project = project("full");
    let root = project.path();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin!("docket"))
        .current_dir(root)
        .args(["watch", "--interval-ms", "50", "--debounce-ms", "50"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let result = (|| -> bool {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let mut document = example();
        document["info"]["description"] = json!("watched change");
        util::write_json(&root.join("openapi.json"), &document).unwrap();
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if let Ok(documents) = store::head(root) {
                if documents.get("users-api") == Some(&document) {
                    return true;
                }
            }
        }
        false
    })();
    std::thread::sleep(std::time::Duration::from_millis(150));
    child.kill().unwrap();
    let _ = child.wait();
    assert!(result, "watch did not sync changed inputs");
    assert_eq!(store::revisions(root).unwrap().len(), 2);
}
