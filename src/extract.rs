use crate::{
    config::{Config, Source},
    openapi, util,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

pub type Documents = BTreeMap<String, Value>;

#[derive(Debug, Default, Clone, Serialize)]
pub struct Timings {
    pub extraction_ms: f64,
    pub import_ms: Option<f64>,
    pub generation_ms: Option<f64>,
    pub normalization_ms: f64,
}

pub fn extract(
    root: &Path,
    config: &Config,
    original_root: Option<&Path>,
) -> Result<(Documents, BTreeMap<String, Timings>)> {
    let mut documents = BTreeMap::new();
    let mut timings = BTreeMap::new();
    for (name, service) in &config.services {
        let start = Instant::now();
        let timeout = Duration::from_secs(config.timeout_seconds);
        let mut timing = Timings::default();
        let (document, base) = match &service.source {
            Source::File { path } => {
                let path = root.join(path);
                ensure!(
                    path.canonicalize()?.starts_with(root.canonicalize()?),
                    "OpenAPI file must be inside the project"
                );
                (util::read_json(&path)?, path)
            }
            Source::Command { command, cwd } => {
                let directory = source_cwd(root, cwd.as_deref())?;
                let mut process = Command::new(&command[0]);
                process.args(&command[1..]).current_dir(&directory);
                process.env("DOCKET_SOURCE_ROOT", root);
                if let Some(original) = original_root {
                    process.env("DOCKET_ORIGINAL_ROOT", original);
                }
                let bytes = util::run_checked(&mut process, timeout)
                    .with_context(|| format!("export {name}"))?;
                (
                    serde_json::from_slice(&bytes)
                        .with_context(|| format!("{name}: export command must print JSON only"))?,
                    directory.join("__docket_export__.json"),
                )
            }
            Source::Fastapi {
                app,
                python,
                factory,
                cwd,
                import_paths,
            } => {
                let directory = source_cwd(root, cwd.as_deref())?;
                let interpreter = util::executable(python, original_root.unwrap_or(root))?;
                let original_repository = if let Some(original) = original_root {
                    crate::git::repo_root(original).unwrap_or_else(|_| original.to_owned())
                } else {
                    root.to_owned()
                };
                let settings = json!({"root":root,"original_repository":original_repository,"app":app,"factory":factory,"import_paths":import_paths});
                let output = util::run_checked(
                    Command::new(interpreter)
                        .args([
                            "-c",
                            include_str!("fastapi_adapter.py"),
                            &serde_json::to_string(&settings)?,
                        ])
                        .current_dir(&directory),
                    timeout,
                )
                .with_context(|| format!("extract FastAPI service {name}"))?;
                let envelope: Value =
                    serde_json::from_slice(&output).context("invalid FastAPI adapter response")?;
                timing.import_ms = envelope
                    .pointer("/timings/import_ms")
                    .and_then(Value::as_f64);
                timing.generation_ms = envelope
                    .pointer("/timings/generation_ms")
                    .and_then(Value::as_f64);
                (
                    envelope
                        .get("document")
                        .context("adapter omitted document")?
                        .clone(),
                    directory.join("__docket_fastapi__.json"),
                )
            }
        };
        timing.extraction_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let document = openapi::bundle(root, &base, document)
            .with_context(|| format!("validate service {name}"))?;
        openapi::validate_snapshot(&document)?;
        timing.normalization_ms = start.elapsed().as_secs_f64() * 1000.0;
        documents.insert(name.clone(), document);
        timings.insert(name.clone(), timing);
    }
    Ok((documents, timings))
}

fn source_cwd(root: &Path, cwd: Option<&str>) -> Result<PathBuf> {
    let directory = root.join(cwd.unwrap_or(".")).canonicalize()?;
    ensure!(
        directory.starts_with(root.canonicalize()?),
        "source cwd must be inside the project"
    );
    Ok(directory)
}
