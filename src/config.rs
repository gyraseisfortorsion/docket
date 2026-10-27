use crate::util;
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HistoryMode {
    Head,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub format_version: u32,
    pub history: HistoryMode,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    pub services: BTreeMap<String, Service>,
}
fn default_timeout() -> u64 {
    120
}
fn default_python() -> String {
    "python3".into()
}
fn default_import_paths() -> Vec<String> {
    vec![".".into()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    #[serde(flatten)]
    pub source: Source,
    /// Conservative default: every tracked input. Explicit globs are an optimization.
    #[serde(default)]
    pub inputs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "lowercase", deny_unknown_fields)]
pub enum Source {
    File {
        path: String,
    },
    Command {
        command: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
    },
    Fastapi {
        app: String,
        #[serde(default = "default_python")]
        python: String,
        #[serde(default)]
        factory: bool,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default = "default_import_paths")]
        import_paths: Vec<String>,
    },
}

impl Config {
    pub fn new(history: HistoryMode) -> Self {
        Self {
            format_version: 1,
            history,
            timeout_seconds: default_timeout(),
            services: BTreeMap::new(),
        }
    }
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(".contracts/config.toml");
        let config: Self = toml::from_str(
            &fs::read_to_string(&path)
                .with_context(|| format!("read {}; run contracts init first", path.display()))?,
        )?;
        ensure!(
            config.format_version == 1,
            "unsupported configuration format"
        );
        ensure!(
            config.timeout_seconds > 0,
            "timeout_seconds must be positive"
        );
        let mut names = std::collections::BTreeSet::new();
        for (name, service) in &config.services {
            util::safe_name(name)?;
            ensure!(
                names.insert(name.to_lowercase()),
                "service names must be unique ignoring case"
            );
            for pattern in &service.inputs {
                globset::Glob::new(pattern)?;
            }
            if let Source::Command { command, .. } = &service.source {
                ensure!(!command.is_empty(), "export command for {name} is empty");
            }
        }
        Ok(config)
    }
    pub fn save(&self, root: &Path) -> Result<()> {
        util::write_atomic(
            &root.join(".contracts/config.toml"),
            toml::to_string_pretty(self)?.as_bytes(),
        )
    }
}

pub fn discover(start: &Path) -> Result<PathBuf> {
    for path in start.ancestors() {
        if path.join(".contracts/config.toml").is_file() {
            return Ok(path.to_owned());
        }
    }
    anyhow::bail!("no .contracts/config.toml found; run contracts init");
}
