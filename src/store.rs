use crate::{
    config::{Config, HistoryMode},
    extract::Documents,
    openapi, util,
};
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub snapshot: String,
    pub revision: Option<String>,
    pub services: BTreeMap<String, String>,
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub format_version: u32,
    pub id: String,
    pub parent: Option<String>,
    pub created_at: String,
    pub snapshot: String,
    pub services: BTreeMap<String, String>,
}

pub struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub fn lock(root: &Path) -> Result<Lock> {
    let directory = root.join(".contracts/local");
    fs::create_dir_all(&directory)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("lock"))?;
    file.try_lock_exclusive()
        .context("another contracts operation is running")?;
    recover(root)?;
    Ok(Lock(file))
}

fn recover(root: &Path) -> Result<()> {
    let backup = root.join(".contracts/local/head-backup");
    let head = root.join(".contracts/head");
    if backup.exists() {
        if !head.exists() {
            fs::rename(&backup, &head)?;
        } else {
            fs::remove_dir_all(&backup)?;
        }
    }
    Ok(())
}

pub fn manifest(root: &Path) -> Result<Option<Manifest>> {
    let path = root.join(".contracts/head/manifest.json");
    if !path.exists() {
        ensure!(
            util::files(&root.join(".contracts/head"))?.is_empty(),
            "head is missing its manifest; restore it from Git or history"
        );
        return Ok(None);
    }
    let manifest: Manifest = util::read_json(&path)?;
    ensure!(manifest.format_version == 1, "unsupported snapshot format");
    Ok(Some(manifest))
}

pub fn verify(root: &Path) -> Result<()> {
    let Some(manifest) = manifest(root)? else {
        return Ok(());
    };
    let head = root.join(".contracts/head");
    let mut actual = BTreeSet::new();
    for path in util::files(&head)? {
        let name = path.to_string_lossy().replace('\\', "/");
        if name == "manifest.json" {
            continue;
        }
        actual.insert(name.clone());
        let expected = manifest.files.get(&name).with_context(|| {
            format!("unrecognized generated file {name}; move edits into a proposal")
        })?;
        ensure!(
            &util::hash(&fs::read(head.join(path))?) == expected,
            "generated head file {name} was modified; restore it or move edits into a proposal before syncing"
        );
    }
    let expected: BTreeSet<_> = manifest.files.keys().cloned().collect();
    ensure!(
        actual == expected,
        "generated head files were deleted; restore them before syncing"
    );
    Ok(())
}

pub fn head(root: &Path) -> Result<Documents> {
    verify(root)?;
    let Some(manifest) = manifest(root)? else {
        return Ok(BTreeMap::new());
    };
    let mut documents = BTreeMap::new();
    for (name, digest) in &manifest.services {
        util::safe_name(name)?;
        let doc: serde_json::Value =
            util::read_json(&root.join(".contracts/head").join(name).join("openapi.json"))?;
        ensure!(
            util::hash(&util::json_bytes(&doc)?) == *digest,
            "snapshot hash mismatch for {name}"
        );
        documents.insert(name.clone(), doc);
    }
    Ok(documents)
}

pub fn content_hashes(documents: &Documents) -> Result<BTreeMap<String, String>> {
    documents
        .iter()
        .map(|(name, doc)| Ok((name.clone(), util::hash(&util::json_bytes(doc)?))))
        .collect()
}

pub fn snapshot_id(documents: &Documents) -> Result<String> {
    Ok(util::hash(&serde_json::to_vec(&content_hashes(
        documents,
    )?)?))
}

fn generated(documents: &Documents) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    for (name, doc) in documents {
        util::safe_name(name)?;
        files.insert(
            PathBuf::from(name).join("openapi.json"),
            util::json_bytes(doc)?,
        );
        for (path, bytes) in openapi::views(name, doc)? {
            files.insert(PathBuf::from(name).join(path), bytes);
        }
    }
    Ok(files)
}

#[derive(Debug, Serialize)]
pub struct SyncResult {
    pub changed: bool,
    pub snapshot: String,
    pub revision: Option<String>,
    pub storage_ms: f64,
}

/// Validate and prepare everything before replacing head. Archive objects are immutable;
/// interrupted writes may leave unreachable objects, but never publish a partial head.
pub fn sync(root: &Path, config: &Config, documents: &Documents) -> Result<SyncResult> {
    let start = Instant::now();
    verify(root)?;
    let previous = manifest(root)?;
    let services = content_hashes(documents)?;
    let snapshot = snapshot_id(documents)?;
    let needs_archive = config.history == HistoryMode::Full
        && previous
            .as_ref()
            .and_then(|m| m.revision.as_ref())
            .is_none();
    if previous.as_ref().is_some_and(|m| m.snapshot == snapshot) && !needs_archive {
        return Ok(SyncResult {
            changed: false,
            snapshot,
            revision: previous.and_then(|m| m.revision),
            storage_ms: start.elapsed().as_secs_f64() * 1000.0,
        });
    }
    for document in documents.values() {
        openapi::validate_snapshot(document)?;
    }
    let mut files = generated(documents)?;
    let revision = if config.history == HistoryMode::Full {
        let created_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        let parent = previous.as_ref().and_then(|m| m.revision.clone());
        let id = util::hash(format!("{snapshot}\n{parent:?}\n{created_at}").as_bytes());
        Some(Revision {
            format_version: 1,
            id,
            parent,
            created_at,
            snapshot: snapshot.clone(),
            services: services.clone(),
        })
    } else {
        None
    };
    let manifest = Manifest {
        format_version: 1,
        snapshot: snapshot.clone(),
        revision: revision.as_ref().map(|r| r.id.clone()),
        services,
        files: files
            .iter()
            .map(|(path, bytes)| (path.to_string_lossy().replace('\\', "/"), util::hash(bytes)))
            .collect(),
    };
    files.insert(PathBuf::from("manifest.json"), util::json_bytes(&manifest)?);
    let staging = tempfile::tempdir_in(root.join(".contracts/local"))?;
    let new_head = staging.path().join("head");
    fs::create_dir_all(&new_head)?;
    for (path, bytes) in files {
        util::write_prepared(&new_head.join(path), &bytes)?;
    }
    if let Some(revision) = &revision {
        for (name, doc) in documents {
            let digest = &revision.services[name];
            let path = root
                .join(".contracts/objects")
                .join(format!("{digest}.json.zst"));
            if !path.exists() {
                let bytes = zstd::stream::encode_all(util::json_bytes(doc)?.as_slice(), 3)?;
                util::write_atomic(&path, &bytes)?;
            } else {
                let bytes = zstd::stream::decode_all(fs::read(&path)?.as_slice())?;
                ensure!(
                    util::hash(&bytes) == *digest,
                    "corrupted archive object {digest}"
                );
            }
        }
        util::write_json(
            &root
                .join(".contracts/history")
                .join(format!("{}.json", revision.id)),
            revision,
        )?;
    }
    let current_head = root.join(".contracts/head");
    let backup = root.join(".contracts/local/head-backup");
    let existed = current_head.exists();
    if existed {
        fs::rename(&current_head, &backup)?;
    }
    if let Err(error) = fs::rename(&new_head, &current_head) {
        if existed {
            fs::rename(&backup, &current_head)?;
        }
        return Err(error.into());
    }
    if existed {
        fs::remove_dir_all(backup)?;
    }
    Ok(SyncResult {
        changed: true,
        snapshot,
        revision: manifest.revision,
        storage_ms: start.elapsed().as_secs_f64() * 1000.0,
    })
}

pub fn revisions(root: &Path) -> Result<Vec<Revision>> {
    let mut records: Vec<Revision> = util::files(&root.join(".contracts/history"))?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .map(|path| util::read_json(&root.join(".contracts/history").join(path)))
        .collect::<Result<_>>()?;
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
    Ok(records)
}

pub fn revision(root: &Path, prefix: &str) -> Result<Revision> {
    ensure!(
        prefix.len() >= 6 && prefix.bytes().all(|b| b.is_ascii_hexdigit()),
        "revision must be a hexadecimal ID or prefix of at least six characters"
    );
    let mut matches: Vec<_> = revisions(root)?
        .into_iter()
        .filter(|r| r.id.starts_with(prefix))
        .collect();
    ensure!(
        matches.len() == 1,
        "unknown or ambiguous revision {prefix}; head-only projects can use git:<ref>"
    );
    Ok(matches.remove(0))
}

pub fn load_revision(root: &Path, prefix: &str) -> Result<Documents> {
    let record = revision(root, prefix)?;
    let mut documents = BTreeMap::new();
    for (name, digest) in record.services {
        ensure!(
            digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid archive hash"
        );
        let bytes = zstd::stream::decode_all(
            fs::read(
                root.join(".contracts/objects")
                    .join(format!("{digest}.json.zst")),
            )?
            .as_slice(),
        )?;
        ensure!(
            util::hash(&bytes) == digest,
            "corrupted archive object {digest}"
        );
        documents.insert(name, serde_json::from_slice(&bytes)?);
    }
    ensure!(
        snapshot_id(&documents)? == record.snapshot,
        "corrupted revision snapshot"
    );
    Ok(documents)
}

pub fn baseline(root: &Path, selector: &str) -> Result<Documents> {
    if selector == "head" || selector == "current" {
        return head(root);
    }
    if let Some(reference) = selector.strip_prefix("git:") {
        return crate::git::baseline(root, reference);
    }
    if selector == "previous" {
        let current = manifest(root)?
            .context("no current snapshot")?
            .revision
            .context("head-only mode has no native parent; use git:<ref>")?;
        let record = revision(root, &current)?;
        let Some(parent) = record.parent else {
            return Ok(Documents::new());
        };
        return load_revision(root, &parent);
    }
    load_revision(root, selector)
}

pub fn ensure_initialized(root: &Path) -> Result<()> {
    if !root.join(".contracts/config.toml").exists() {
        bail!("run contracts init first");
    }
    Ok(())
}
