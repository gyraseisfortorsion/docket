use crate::util;
use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub const VERSION: &str = "1.34.1";

pub fn cache_path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))
        .context("set XDG_CACHE_HOME or HOME to install oasdiff")?;
    Ok(base
        .join("docket")
        .join(format!("oasdiff-{VERSION}"))
        .join(if cfg!(windows) {
            "oasdiff.exe"
        } else {
            "oasdiff"
        }))
}

pub fn verify(path: &Path) -> Result<()> {
    let output = util::run_checked(Command::new(path).arg("--version"), Duration::from_secs(15))?;
    let text = String::from_utf8_lossy(&output);
    ensure!(
        text.split_whitespace()
            .any(|word| word.trim_start_matches('v') == VERSION),
        "expected oasdiff {VERSION}, got {}; run docket engine install",
        text.trim()
    );
    Ok(())
}

pub fn locate() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DOCKET_OASDIFF") {
        let path = PathBuf::from(path);
        verify(&path)?;
        return Ok(path);
    }
    let mut candidates = Vec::new();
    if let Some(parent) = std::env::current_exe()?.parent() {
        candidates.push(parent.join(if cfg!(windows) {
            "oasdiff.exe"
        } else {
            "oasdiff"
        }));
    }
    if let Ok(path) = cache_path() {
        candidates.push(path);
    }
    if let Ok(path) = util::executable("oasdiff", &std::env::current_dir()?) {
        candidates.push(path);
    }
    for path in candidates {
        if path.is_file() {
            verify(&path)?;
            return Ok(path);
        }
    }
    bail!("oasdiff {VERSION} is required for compatibility analysis; run docket engine install");
}

fn asset() -> Result<(&'static str, &'static str)> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64" | "x86_64") => (
            "darwin_all",
            "64948ba30c5555d3d15b95a7f3eda22fa83c15cf37fd4dad1720c0deb0a07cb5",
        ),
        ("linux", "x86_64") => (
            "linux_amd64",
            "75775f65b0adebd26a8e013af51a799342dfdc6262d89efa9e3b7bdf89657d90",
        ),
        ("linux", "aarch64") => (
            "linux_arm64",
            "e8f99bbf102a4b7f235104da72d0b9d67702e78622d1d4d37169e698ee90a249",
        ),
        ("windows", "x86_64") => (
            "windows_amd64",
            "15e6f32ece94244534a61ce4fb1d60912e51e67e363a05bb6561d89b28df73bb",
        ),
        ("windows", "aarch64") => (
            "windows_arm64",
            "4794300521a2ff76d14205f5141884e6f6052212484caf1d3655c149b314492c",
        ),
        other => bail!("no pinned oasdiff release for {other:?}; provide DOCKET_OASDIFF"),
    })
}

pub fn install() -> Result<PathBuf> {
    let destination = cache_path()?;
    if destination.is_file() && verify(&destination).is_ok() {
        return Ok(destination);
    }
    let (platform, checksum) = asset()?;
    let url = format!(
        "https://github.com/oasdiff/oasdiff/releases/download/v{VERSION}/oasdiff_{VERSION}_{platform}.tar.gz"
    );
    let bytes = util::run_checked(
        Command::new("curl").args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--retry",
            "2",
            "--connect-timeout",
            "15",
            "--max-time",
            "120",
            &url,
        ]),
        Duration::from_secs(150),
    )
    .context("download pinned oasdiff (curl is required)")?;
    ensure!(
        util::hash(&bytes) == checksum,
        "oasdiff download checksum mismatch"
    );
    let mut archive = tar::Archive::new(GzDecoder::new(bytes.as_slice()));
    let expected = if cfg!(windows) {
        "oasdiff.exe"
    } else {
        "oasdiff"
    };
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry
            .path()?
            .file_name()
            .is_some_and(|name| name == expected)
            && entry.header().entry_type().is_file()
        {
            use std::io::Read;
            let mut binary = Vec::new();
            entry.read_to_end(&mut binary)?;
            util::write_atomic(&destination, &binary)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))?;
            }
            verify(&destination)?;
            return Ok(destination);
        }
    }
    bail!("oasdiff archive did not contain {expected}");
}

pub fn changelog(
    engine: &Path,
    base: &Value,
    current: &Value,
    timeout: Duration,
) -> Result<Vec<Value>> {
    let directory = tempfile::tempdir()?;
    util::write_json(&directory.path().join("base.json"), base)?;
    util::write_json(&directory.path().join("current.json"), current)?;
    // Do not inherit user/repository oasdiff configuration: findings must be reproducible.
    util::write_atomic(&directory.path().join("config.json"), b"{}")?;
    let output = util::run(
        Command::new(engine)
            .current_dir(directory.path())
            .args([
                "changelog",
                "base.json",
                "current.json",
                "--format",
                "json",
                "--config",
                "config.json",
                "--allow-external-refs=false",
            ])
            .env_remove("OASDIFF_CONFIG"),
        timeout,
    )?;
    ensure!(
        output.success,
        "oasdiff failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let findings: Value =
        serde_json::from_slice(&output.stdout).context("invalid oasdiff JSON output")?;
    if findings.is_null() {
        return Ok(Vec::new());
    }
    Ok(findings
        .as_array()
        .context("oasdiff changelog must return a JSON array")?
        .clone())
}
