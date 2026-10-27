use crate::{
    compare,
    config::Config,
    extract::{self, Documents},
    store::{self, Manifest},
    util,
};
use anyhow::{Context, Result, ensure};
use globset::{Glob, GlobSetBuilder};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(120);
const MARKER: &str = "# contracts-managed-hook v1";

fn command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(root);
    command
}
fn output(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    util::run_checked(command(root).args(args), TIMEOUT)
}
fn string(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(output(root, args)?)?.trim_end().into())
}
fn names(bytes: &[u8]) -> Result<Vec<String>> {
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| Ok(String::from_utf8(p.to_vec())?))
        .collect()
}
pub fn repo_root(root: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(string(root, &["rev-parse", "--show-toplevel"])?).canonicalize()?)
}
fn prefix(root: &Path, repo: &Path) -> Result<String> {
    let prefix = root
        .strip_prefix(repo)?
        .to_string_lossy()
        .replace('\\', "/");
    Ok(if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    })
}

pub fn baseline(root: &Path, reference: &str) -> Result<Documents> {
    ensure!(!reference.is_empty(), "empty Git reference");
    let commit = string(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )?;
    let repo = repo_root(root)?;
    let prefix = prefix(root, &repo)?;
    let manifest_path = format!("{commit}:{prefix}.contracts/head/manifest.json");
    let result = util::run(command(root).args(["show", &manifest_path]), TIMEOUT)?;
    if !result.success {
        return Ok(Documents::new());
    }
    let manifest: Manifest = serde_json::from_slice(&result.stdout)
        .context("invalid snapshot manifest in Git baseline")?;
    ensure!(manifest.format_version == 1, "unsupported baseline format");
    let mut documents = Documents::new();
    for (service, digest) in manifest.services {
        util::safe_name(&service)?;
        let doc: Value = serde_json::from_slice(&output(
            root,
            &[
                "show",
                &format!("{commit}:{prefix}.contracts/head/{service}/openapi.json"),
            ],
        )?)?;
        ensure!(
            util::hash(&util::json_bytes(&doc)?) == digest,
            "corrupted Git baseline for {service}"
        );
        crate::openapi::validate_snapshot(&doc)?;
        documents.insert(service, doc);
    }
    Ok(documents)
}

fn hooks_directory(root: &Path) -> Result<PathBuf> {
    repo_root(root)?;
    let path = PathBuf::from(string(root, &["rev-parse", "--git-path", "hooks"])?);
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}
fn quote(path: &Path) -> String {
    shell_words::quote(&path.to_string_lossy()).into_owned()
}

pub fn install(root: &Path) -> Result<PathBuf> {
    let directory = hooks_directory(root)?;
    fs::create_dir_all(&directory)?;
    let hook = directory.join("pre-commit");
    let backup = directory.join("pre-commit.contracts-original");
    if hook.exists() {
        let content = fs::read_to_string(&hook).unwrap_or_default();
        if !content.contains(MARKER) {
            ensure!(
                !backup.exists(),
                "hook backup already exists: {}",
                backup.display()
            );
            fs::rename(&hook, &backup)?;
        }
    }
    let script = format!(
        "#!/bin/sh\n{MARKER}\nset -e\nif [ -x {} ]; then\n  {} \"$@\"\nfi\nexec {} --root {} hook\n",
        quote(&backup),
        quote(&backup),
        quote(&std::env::current_exe()?),
        quote(root)
    );
    util::write_atomic(&hook, script.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))?;
    }
    Ok(hook)
}
pub fn uninstall(root: &Path) -> Result<()> {
    let directory = hooks_directory(root)?;
    let hook = directory.join("pre-commit");
    if !hook.exists() {
        return Ok(());
    }
    ensure!(
        fs::read_to_string(&hook)?.contains(MARKER),
        "pre-commit is not managed by contracts"
    );
    fs::remove_file(&hook)?;
    let backup = directory.join("pre-commit.contracts-original");
    if backup.exists() {
        fs::rename(backup, hook)?;
    }
    Ok(())
}

fn relevant(config: &Config, paths: &[String], prefix: &str) -> Result<bool> {
    if paths
        .iter()
        .any(|p| p == &format!("{prefix}.contracts/config.toml"))
    {
        return Ok(true);
    }
    for service in config.services.values() {
        let mut builder = GlobSetBuilder::new();
        for pattern in &service.inputs {
            builder.add(Glob::new(pattern)?);
        }
        let matcher = builder.build()?;
        for path in paths {
            if path.starts_with(&format!("{prefix}.contracts/")) {
                continue;
            }
            let relative = path.strip_prefix(prefix).unwrap_or(path);
            if service.inputs.is_empty() || matcher.is_match(relative) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn hook(root: &Path) -> Result<()> {
    let _lock = store::lock(root)?;
    store::verify(root)?;
    let repo = repo_root(root)?;
    let prefix = prefix(root, &repo)?;
    let staged = names(&output(root, &["diff", "--cached", "--name-only", "-z"])?)?;
    let indexed_config = output(root, &["show", &format!(":{prefix}.contracts/config.toml")])
        .context("stage .contracts/config.toml before committing")?;
    let config: Config = toml::from_str(std::str::from_utf8(&indexed_config)?)?;
    let needs_extract = relevant(&config, &staged, &prefix)?;
    let generated_changed = staged.iter().any(|p| {
        ["head/", "objects/", "history/"]
            .iter()
            .any(|part| p.starts_with(&format!("{prefix}.contracts/{part}")))
    });
    if !needs_extract && !generated_changed {
        return Ok(());
    }

    let index_name = string(&repo, &["rev-parse", "--git-path", "index"])?;
    let index = if Path::new(&index_name).is_absolute() {
        PathBuf::from(index_name)
    } else {
        repo.join(index_name)
    };
    let original_index = fs::read(&index).context("read active Git index")?;
    ensure!(
        output(root, &["ls-files", "--unmerged", "-z"])?.is_empty(),
        "resolve Git conflicts before generating contracts"
    );
    // Submodule contents are not part of the superproject's index export.
    let entries = output(root, &["ls-files", "--stage"])?;
    ensure!(
        !String::from_utf8_lossy(&entries)
            .lines()
            .any(|line| line.starts_with("160000 ")),
        "staged-tree extraction does not support submodules; use a pre-bundled OpenAPI file in a repository without gitlinks"
    );
    let temporary = tempfile::tempdir()?;
    let export = temporary.path().join("tree");
    fs::create_dir_all(&export)?;
    let export_prefix = format!("{}/", export.display());
    output(
        &repo,
        &[
            "checkout-index",
            "--all",
            &format!("--prefix={export_prefix}"),
        ],
    )?;
    let staged_root = export.join(root.strip_prefix(&repo)?);
    let staged_config = Config::load(&staged_root)?;
    store::verify(&staged_root)?;
    if !needs_extract {
        return Ok(());
    }
    let before = if util::run(
        command(root).args(["rev-parse", "--verify", "HEAD"]),
        TIMEOUT,
    )?
    .success
    {
        baseline(root, "HEAD")?
    } else {
        Documents::new()
    };
    let (documents, _) = extract::extract(&staged_root, &staged_config, Some(root))?;
    let comparison = compare::compare(
        &before,
        &documents,
        Duration::from_secs(staged_config.timeout_seconds),
    )?;
    {
        let _staged_lock = store::lock(&staged_root)?;
        store::sync(&staged_root, &staged_config, &documents)?;
    }
    // Prepare a separate index using the exported work tree. Unrelated staged entries
    // are retained byte-for-byte in content; no git add ever sees the real source tree.
    let candidate = temporary.path().join("index");
    fs::write(&candidate, &original_index)?;
    let git_dir = string(&repo, &["rev-parse", "--absolute-git-dir"])?;
    let artifact_paths: Vec<String> = ["head", "history", "objects"]
        .iter()
        .map(|part| format!("{prefix}.contracts/{part}"))
        .filter(|p| export.join(p).exists())
        .collect();
    let mut add = Command::new("git");
    add.current_dir(&repo)
        .args(["--git-dir", &git_dir, "--work-tree"])
        .arg(&export)
        .args(["-c", "core.autocrlf=false", "add", "-A", "--"])
        .args(&artifact_paths)
        .env("GIT_INDEX_FILE", &candidate);
    util::run_checked(&mut add, TIMEOUT)?;
    let baseline_index = temporary.path().join("baseline-index");
    fs::write(&baseline_index, &original_index)?;
    let old_tree = String::from_utf8(util::run_checked(
        command(&repo)
            .arg("write-tree")
            .env("GIT_INDEX_FILE", &baseline_index),
        TIMEOUT,
    )?)?
    .trim()
    .to_owned();
    let new_tree = String::from_utf8(util::run_checked(
        command(&repo)
            .arg("write-tree")
            .env("GIT_INDEX_FILE", &candidate),
        TIMEOUT,
    )?)?
    .trim()
    .to_owned();
    let changed = names(&output(
        &repo,
        &[
            "diff",
            "--name-only",
            "-z",
            &old_tree,
            &new_tree,
            "--",
            &format!("{prefix}.contracts"),
        ],
    )?)?;
    if !changed.is_empty() {
        publish(
            root,
            &repo,
            &export,
            &index,
            &original_index,
            &candidate,
            &changed,
        )?;
        eprintln!(
            "contracts: updated staged contracts ({} files)",
            changed.len()
        );
    }
    if comparison.breaking() > 0 {
        eprintln!(
            "contracts: WARNING: {} breaking API change(s); commit is allowed",
            comparison.breaking()
        );
        for finding in comparison
            .findings
            .iter()
            .filter(|f| f.classification == "breaking")
            .take(20)
        {
            eprintln!(
                "  {}: {}",
                finding.service,
                finding
                    .detail
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("breaking change")
            );
        }
    }
    Ok(())
}

struct IndexLock(Option<PathBuf>);
impl Drop for IndexLock {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn publish(
    root: &Path,
    repo: &Path,
    export: &Path,
    index: &Path,
    original_index: &[u8],
    candidate: &Path,
    changed: &[String],
) -> Result<()> {
    let prepared = tempfile::tempdir_in(root.join(".contracts/local"))?;
    let prepared_head = prepared.path().join("head");
    let staged_head = export
        .join(root.strip_prefix(repo)?)
        .join(".contracts/head");
    fs::create_dir_all(&prepared_head)?;
    for path in util::files(&staged_head)? {
        util::write_prepared(
            &prepared_head.join(&path),
            &fs::read(staged_head.join(path))?,
        )?;
    }
    store::verify(root)?;
    let lock_path = PathBuf::from(format!("{}.lock", index.display()));
    let mut index_lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .context("Git index is locked by another process")?;
    let mut guard = IndexLock(Some(lock_path.clone()));
    ensure!(
        fs::read(index)? == original_index,
        "Git index changed during extraction; retry the commit"
    );
    let head = root.join(".contracts/head");
    let saved_head = root.join(".contracts/local/hook-head-backup");
    ensure!(
        !saved_head.exists(),
        "unfinished hook backup at {}; restore it before retrying",
        saved_head.display()
    );
    let had_head = head.exists();
    if had_head {
        fs::rename(&head, &saved_head)?;
    }
    let mut backups = Vec::new();
    let result = (|| -> Result<()> {
        fs::rename(&prepared_head, &head)?;
        let head_prefix = format!("{}.contracts/head/", prefix(root, repo)?);
        for path in changed {
            if path.starts_with(&head_prefix) {
                continue;
            }
            let destination = repo.join(path);
            let previous = if destination.exists() {
                Some(fs::read(&destination)?)
            } else {
                None
            };
            backups.push((destination.clone(), previous));
            let source = export.join(path);
            if source.exists() {
                util::write_atomic(&destination, &fs::read(source)?)?;
            } else if destination.exists() {
                fs::remove_file(destination)?;
            }
        }
        index_lock.write_all(&fs::read(candidate)?)?;
        index_lock.sync_all()?;
        fs::rename(&lock_path, index)?;
        // The pathname is now free for another Git process to lock.
        guard.0 = None;
        Ok(())
    })();
    if let Err(error) = result {
        if head.exists() {
            fs::remove_dir_all(&head)?;
        }
        if had_head {
            fs::rename(&saved_head, &head)?;
        }
        for (path, bytes) in backups {
            if let Some(bytes) = bytes {
                util::write_atomic(&path, &bytes)?;
            } else if path.exists() {
                fs::remove_file(path)?;
            }
        }
        return Err(error);
    }
    if had_head {
        fs::remove_dir_all(saved_head)?;
    }
    Ok(())
}
