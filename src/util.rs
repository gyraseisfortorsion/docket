use anyhow::{Context, Result, bail};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use wait_timeout::ChildExt;

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn json_bytes(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("parse {}", path.display()))
}

/// Replace a file atomically, leaving its mtime untouched when bytes are unchanged.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    let parent = path.parent().context("file has no parent")?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    write_atomic(path, &json_bytes(value)?)
}

/// Write inside an unpublished temporary tree. Canonical documents and their
/// manifest are durable; derived views need no per-file atomic rename/fsync.
pub fn write_prepared(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(path.parent().context("prepared file has no parent")?)?;
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    if path
        .file_name()
        .is_some_and(|name| name == "openapi.json" || name == "manifest.json")
    {
        file.sync_all()?;
    }
    Ok(())
}

pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
    pub code: Option<i32>,
}

/// File-backed capture avoids deadlocks on large OpenAPI documents.
pub fn run(command: &mut Command, timeout: Duration) -> Result<Output> {
    let stdout = tempfile::NamedTempFile::new()?;
    let stderr = tempfile::NamedTempFile::new()?;
    command
        .stdin(Stdio::null())
        .stdout(stdout.reopen()?)
        .stderr(stderr.reopen()?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("start {command:?}"))?;
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "command timed out after {}s: {command:?}",
                timeout.as_secs()
            );
        }
    };
    Ok(Output {
        stdout: fs::read(stdout.path())?,
        stderr: fs::read(stderr.path())?,
        success: status.success(),
        code: status.code(),
    })
}

pub fn run_checked(command: &mut Command, timeout: Duration) -> Result<Vec<u8>> {
    let output = run(command, timeout)?;
    if !output.success {
        bail!(
            "command failed ({:?}): {}",
            output.code,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

pub fn executable(program: &str, root: &Path) -> Result<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    if path.components().count() > 1 {
        return Ok(root.join(path));
    }
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Ok(candidate);
        }
        #[cfg(windows)]
        if candidate.with_extension("exe").is_file() {
            return Ok(candidate.with_extension("exe"));
        }
    }
    bail!("cannot find executable {program:?} on PATH");
}

pub fn safe_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 100
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'),
        "invalid name {name:?}: use letters, digits, dots, hyphens, and underscores (max 100)"
    );
    Ok(())
}

pub fn slug(value: &str) -> String {
    let mut result = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c.to_ascii_lowercase());
        } else if !result.is_empty() && !result.ends_with('-') {
            result.push('-');
        }
        if result.len() >= 70 {
            break;
        }
    }
    let result = result.trim_matches('-');
    if result.is_empty() {
        "default".into()
    } else {
        result.into()
    }
}

pub fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        anyhow::ensure!(
            !entry.file_type().is_symlink(),
            "unexpected symlink {}",
            entry.path().display()
        );
        if entry.file_type().is_file() {
            paths.push(entry.path().strip_prefix(root)?.to_owned());
        }
    }
    paths.sort();
    Ok(paths)
}
