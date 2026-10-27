use crate::{compare, extract::Documents, openapi, store, util};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub format_version: u32,
    pub name: String,
    pub created_at: String,
    pub base_snapshot: String,
    pub services: Vec<String>,
    pub closed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drift {
    pub name: String,
    pub base_snapshot: String,
    pub changes: Vec<compare::Change>,
}

fn directory(root: &Path, name: &str) -> Result<PathBuf> {
    util::safe_name(name)?;
    Ok(root.join(".contracts/proposals").join(name))
}

pub fn create(root: &Path, name: &str) -> Result<()> {
    let destination = directory(root, name)?;
    ensure!(!destination.exists(), "proposal {name} already exists");
    let documents = store::head(root)?;
    ensure!(
        !documents.is_empty(),
        "sync contracts before creating a proposal"
    );
    let staging = tempfile::tempdir_in(root.join(".contracts/local"))?;
    let proposal = Proposal {
        format_version: 1,
        name: name.into(),
        created_at: Utc::now().to_rfc3339(),
        base_snapshot: store::snapshot_id(&documents)?,
        services: documents.keys().cloned().collect(),
        closed: false,
    };
    for (service, document) in &documents {
        util::write_json(
            &staging
                .path()
                .join("base")
                .join(service)
                .join("openapi.json"),
            document,
        )?;
        util::write_json(&staging.path().join(service).join("openapi.json"), document)?;
    }
    util::write_json(&staging.path().join("proposal.json"), &proposal)?;
    fs::create_dir_all(destination.parent().context("proposal parent")?)?;
    fs::rename(staging.path(), &destination)?;
    Ok(())
}

pub fn list(root: &Path) -> Result<Vec<Proposal>> {
    let path = root.join(".contracts/proposals");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut output = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("proposal.json").is_file() {
            let proposal: Proposal = util::read_json(&entry.path().join("proposal.json"))?;
            ensure!(proposal.format_version == 1, "unsupported proposal format");
            util::safe_name(&proposal.name)?;
            output.push(proposal);
        }
    }
    output.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(output)
}

pub fn load(root: &Path, name: &str) -> Result<(Proposal, Documents)> {
    let directory = directory(root, name)?;
    let proposal: Proposal = util::read_json(&directory.join("proposal.json"))?;
    ensure!(
        proposal.format_version == 1 && proposal.name == name,
        "invalid proposal metadata"
    );
    let mut documents = Documents::new();
    for service in &proposal.services {
        util::safe_name(service)?;
        let path = directory.join(service).join("openapi.json");
        let document: Value = util::read_json(&path)?;
        let document = openapi::bundle(root, &path, document)?;
        openapi::validate_snapshot(&document)?;
        documents.insert(service.clone(), document);
    }
    Ok((proposal, documents))
}

pub fn close(root: &Path, name: &str) -> Result<()> {
    let path = directory(root, name)?.join("proposal.json");
    let mut proposal: Proposal = util::read_json(&path)?;
    proposal.closed = true;
    util::write_json(&path, &proposal)
}

pub fn drifts(root: &Path, observed: &Documents) -> Result<Vec<Drift>> {
    let mut output = Vec::new();
    for proposal in list(root)?.into_iter().filter(|p| !p.closed) {
        let (_, desired) = load(root, &proposal.name)?;
        output.push(Drift {
            name: proposal.name,
            base_snapshot: proposal.base_snapshot,
            changes: compare::structural(&desired, observed)?.changes,
        });
    }
    Ok(output)
}

pub fn edit(
    root: &Path,
    name: &str,
    service: Option<&str>,
    operation: Option<&str>,
    schema: Option<&str>,
) -> Result<()> {
    let (proposal, mut documents) = load(root, name)?;
    ensure!(!proposal.closed, "proposal {name} is closed");
    let service = match service {
        Some(name) => name.to_owned(),
        None => {
            ensure!(
                documents.len() == 1,
                "select --service in a multi-service proposal"
            );
            documents.keys().next().context("empty proposal")?.clone()
        }
    };
    let document = documents
        .get_mut(&service)
        .context("unknown service in proposal")?;
    let pointer = if let Some(schema) = schema {
        format!("/components/schemas/{}", util::escape_pointer(schema))
    } else if let Some(operation) = operation {
        let (method, path) = operation
            .split_once(' ')
            .context("operation must be 'METHOD /path'")?;
        let method = method.to_lowercase();
        ensure!(
            openapi::METHODS.contains(&method.as_str()),
            "unknown HTTP method"
        );
        let mut base = format!("/paths/{}", util::escape_pointer(path));
        let mut seen = std::collections::BTreeSet::new();
        while let Some(reference) = document
            .pointer(&base)
            .and_then(|v| v.get("$ref"))
            .and_then(Value::as_str)
        {
            ensure!(seen.insert(base.clone()), "cyclic path reference");
            base = reference
                .strip_prefix('#')
                .context("unbundled path reference")?
                .to_owned();
        }
        format!("{base}/{method}")
    } else {
        String::new()
    };
    let value = document
        .pointer(&pointer)
        .with_context(|| format!("cannot find {pointer} in {service}"))?;
    let temporary = tempfile::Builder::new()
        .prefix("proposal-edit-")
        .suffix(".json")
        .tempfile_in(root.join(".contracts/local"))?;
    util::write_json(temporary.path(), value)?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .context("set EDITOR or VISUAL to edit a proposal")?;
    let words = shell_words::split(&editor)?;
    ensure!(!words.is_empty(), "editor command is empty");
    let status = Command::new(&words[0])
        .args(&words[1..])
        .arg(temporary.path())
        .status()?;
    ensure!(
        status.success(),
        "editor exited unsuccessfully; proposal unchanged"
    );
    let result = (|| -> Result<()> {
        let edited: Value = util::read_json(temporary.path())?;
        *document
            .pointer_mut(&pointer)
            .context("edit target disappeared")? = edited;
        openapi::validate_snapshot(document)?;
        util::write_json(
            &directory(root, name)?.join(&service).join("openapi.json"),
            document,
        )
    })();
    if let Err(error) = result {
        let (_, path) = temporary.keep()?;
        return Err(error).with_context(|| {
            format!(
                "proposal unchanged; recover your edit from {}",
                path.display()
            )
        });
    }
    Ok(())
}
