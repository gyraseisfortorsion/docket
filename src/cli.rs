use crate::{
    compare,
    config::{self, Config, HistoryMode, Service, Source},
    engine,
    extract::{self, Documents},
    git, openapi, proposal,
    report::{self, Format},
    store, util,
};
use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(
    name = "docket",
    version,
    about = "Local, semantic version control for OpenAPI contracts"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Project directory (otherwise search upward)"
    )]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Register a service. Run sync to capture its first snapshot.
    Init(Init),
    /// Extract and save contracts only when their content changes.
    Sync {
        #[arg(long)]
        timings: bool,
        #[arg(long)]
        json: bool,
    },
    /// Inspect source changes and proposal drift without saving a snapshot.
    Status {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        timings: bool,
    },
    /// Compare head with fresh source, or explicit snapshots.
    Diff {
        #[arg(long, conflicts_with = "proposal")]
        from: Option<String>,
        #[arg(long, conflicts_with = "proposal")]
        to: Option<String>,
        #[arg(long)]
        proposal: Option<String>,
        #[arg(long, value_enum, default_value = "txt")]
        format: Format,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Inspect portable native snapshot history.
    Log {
        #[arg(long)]
        json: bool,
    },
    /// Print an operation, schema, or full OpenAPI document.
    Show {
        #[arg(long)]
        service: Option<String>,
        #[arg(long, conflicts_with = "schema")]
        operation: Option<String>,
        #[arg(long)]
        schema: Option<String>,
        #[arg(long, default_value = "head")]
        at: String,
    },
    /// Produce an inventory, change report, and open proposal drift.
    Report {
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        working: bool,
        #[arg(long, value_enum, default_value = "markdown")]
        format: Format,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Fresh extraction and CI policy enforcement (also rejects stale head).
    Check {
        #[arg(long)]
        against: String,
        #[arg(long, value_enum, default_value = "breaking")]
        fail_on: Policy,
        #[arg(long)]
        json: bool,
    },
    /// Create a named, editable desired API snapshot.
    Propose { name: String },
    /// Edit a proposal in EDITOR or VISUAL; defaults to the full document.
    Edit {
        name: String,
        #[arg(long)]
        service: Option<String>,
        #[arg(long, conflicts_with = "schema")]
        operation: Option<String>,
        #[arg(long)]
        schema: Option<String>,
    },
    /// List proposals, including explicitly closed proposals.
    Proposals {
        #[arg(long)]
        json: bool,
    },
    /// Mark a proposal closed without deleting it.
    Close { name: String },
    /// Debounced development sync (Ctrl-C to stop).
    Watch {
        #[arg(long,default_value_t=500,value_parser=clap::value_parser!(u64).range(50..))]
        interval_ms: u64,
        #[arg(long, default_value_t = 300)]
        debounce_ms: u64,
    },
    /// Install or remove opt-in Git integration.
    Hooks {
        #[command(subcommand)]
        command: HookCommand,
    },
    /// Install or locate the pinned compatibility engine.
    Engine {
        #[command(subcommand)]
        command: EngineCommand,
    },
    #[command(hide = true)]
    Hook,
}
#[derive(Clone, Copy, ValueEnum)]
enum Policy {
    Breaking,
    Change,
    Drift,
}
#[derive(Subcommand)]
enum HookCommand {
    Install,
    Uninstall,
}
#[derive(Subcommand)]
enum EngineCommand {
    Install,
    Path,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("source").required(true).args(["file","command","fastapi"])))]
struct Init {
    #[arg(long)]
    service: String,
    #[arg(long)]
    file: Option<String>,
    /// Quoted argv string, parsed without a shell; stdout must be OpenAPI JSON.
    #[arg(long)]
    command: Option<String>,
    #[arg(long)]
    fastapi: Option<String>,
    #[arg(long, default_value = "python3")]
    python: String,
    #[arg(long, requires = "fastapi")]
    factory: bool,
    #[arg(long, value_enum)]
    history: Option<HistoryMode>,
    #[arg(long)]
    cwd: Option<String>,
    #[arg(long)]
    import_path: Vec<String>,
    #[arg(long)]
    input: Vec<String>,
}

pub fn run() -> Result<i32> {
    let cli = Cli::parse();
    if let Action::Engine { command } = cli.command {
        let path = match command {
            EngineCommand::Install => engine::install()?,
            EngineCommand::Path => engine::locate()?,
        };
        println!("{}", path.display());
        return Ok(0);
    }
    let start = cli
        .root
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    if let Action::Init(args) = cli.command {
        init(&start, args)?;
        return Ok(0);
    }
    let root = config::discover(&start)?;
    if let Action::Hook = cli.command {
        git::hook(&root)?;
        return Ok(0);
    }
    if let Action::Watch {
        interval_ms,
        debounce_ms,
    } = cli.command
    {
        return watch(&root, interval_ms, debounce_ms);
    }
    let _lock = store::lock(&root)?;
    let config = Config::load(&root)?;
    let timeout = Duration::from_secs(config.timeout_seconds);
    match cli.command {
        Action::Sync {
            timings,
            json: as_json,
        } => {
            store::verify(&root)?;
            let (documents, extraction) = extract::extract(&root, &config, None)?;
            let result = store::sync(&root, &config, &documents)?;
            if as_json {
                print_json(&json!({"sync":result,"timings":extraction}))?;
            } else {
                println!(
                    "{} {}",
                    if result.changed {
                        "Saved snapshot"
                    } else {
                        "Unchanged snapshot"
                    },
                    &result.snapshot[..12]
                );
            }
            if timings {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"services":extraction,"storage_ms":result.storage_ms})
                    )?
                );
            }
        }
        Action::Status {
            json: as_json,
            timings,
        } => {
            let before = store::head(&root)?;
            let (current, extraction) = extract::extract(&root, &config, None)?;
            let comparison = compare::structural(&before, &current)?;
            let proposals = proposal::drifts(&root, &current)?;
            if as_json {
                print_json(
                    &json!({"changes":comparison.changes,"proposals":proposals,"timings":extraction}),
                )?;
            } else {
                println!(
                    "{} contract change(s); {} open proposal(s)",
                    comparison.changes.len(),
                    proposals.len()
                );
                for change in &comparison.changes {
                    println!("  {} {} {}", change.service, change.kind, change.pointer);
                }
                for drift in &proposals {
                    println!(
                        "  proposal {}: {} drift change(s)",
                        drift.name,
                        drift.changes.len()
                    );
                }
            }
            if timings {
                eprintln!("{}", serde_json::to_string_pretty(&extraction)?);
            }
        }
        Action::Diff {
            from,
            to,
            proposal: proposal_name,
            format,
            output,
        } => {
            store::verify(&root)?;
            let (label, before, current) = if let Some(name) = proposal_name {
                let (_, desired) = proposal::load(&root, &name)?;
                (
                    format!("proposal:{name} (desired → observed)"),
                    desired,
                    extract::extract(&root, &config, None)?.0,
                )
            } else {
                let label = from.unwrap_or_else(|| "head".into());
                let before = select(&root, &config, &label)?;
                let current = select(&root, &config, to.as_deref().unwrap_or("working"))?;
                (label, before, current)
            };
            let comparison = compare::compare(&before, &current, timeout)?;
            let result = report::build_changes(
                &label,
                &before,
                &current,
                comparison,
                proposal::drifts(&root, &current)?,
            )?;
            emit(&root, output, report::render(&result, format)?)?;
        }
        Action::Report {
            since,
            working,
            format,
            output,
        } => {
            let current = if working {
                extract::extract(&root, &config, None)?.0
            } else {
                store::head(&root)?
            };
            ensure!(
                !current.is_empty(),
                "no saved contracts; run docket sync or use --working"
            );
            let label = since.unwrap_or_else(|| "head".into());
            let before = store::baseline(&root, &label)?;
            let comparison = compare::compare(&before, &current, timeout)?;
            let result = report::build(
                &label,
                &before,
                &current,
                comparison,
                proposal::drifts(&root, &current)?,
            )?;
            emit(&root, output, report::render(&result, format)?)?;
        }
        Action::Check {
            against,
            fail_on,
            json: as_json,
        } => {
            let observed = store::head(&root)?;
            let current = extract::extract(&root, &config, None)?.0;
            let stale = observed != current;
            let before = store::baseline(&root, &against)?;
            let comparison = compare::compare(&before, &current, timeout)?;
            let drifts = proposal::drifts(&root, &current)?;
            let failed = stale
                || match fail_on {
                    Policy::Breaking => comparison.breaking() > 0,
                    Policy::Change => !comparison.changes.is_empty(),
                    Policy::Drift => drifts.iter().any(|d| !d.changes.is_empty()),
                };
            if as_json {
                print_json(
                    &json!({"passed":!failed,"stale_head":stale,"comparison":comparison,"proposals":drifts}),
                )?;
            } else {
                println!(
                    "{}: {} breaking finding(s), {} change(s), stale head: {}",
                    if failed { "FAIL" } else { "PASS" },
                    comparison.breaking(),
                    comparison.changes.len(),
                    stale
                );
                if stale {
                    eprintln!("run docket sync and commit the generated contracts");
                }
                for finding in &comparison.findings {
                    println!(
                        "  {} {}: {}",
                        finding.service,
                        finding.classification,
                        finding
                            .detail
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("compatibility finding")
                    );
                }
            }
            return Ok(if failed { 1 } else { 0 });
        }
        Action::Log { json: as_json } => {
            let records = store::revisions(&root)?;
            if as_json {
                print_json(&records)?;
            } else if records.is_empty() {
                println!("No native history. Git baselines remain available through git:<ref>.");
            } else {
                for record in records {
                    println!(
                        "{} {} {} service(s) snapshot {}",
                        &record.id[..12],
                        record.created_at,
                        record.services.len(),
                        &record.snapshot[..12]
                    );
                }
            }
        }
        Action::Show {
            service,
            operation,
            schema,
            at,
        } => {
            let documents = select(&root, &config, &at)?;
            let name = service_name(&documents, service.as_deref())?;
            let document = &documents[&name];
            if let Some(identity) = operation {
                let operations = openapi::operations(document)?;
                print_json(
                    operations
                        .get(&identity)
                        .context("operation not found; use 'METHOD /path'")?,
                )?;
            } else if let Some(schema) = schema {
                print_json(
                    document
                        .pointer(&format!(
                            "/components/schemas/{}",
                            util::escape_pointer(&schema)
                        ))
                        .context("schema not found")?,
                )?;
            } else {
                print_json(document)?;
            }
        }
        Action::Propose { name } => {
            proposal::create(&root, &name)?;
            println!("Created proposal {name}; edit with docket edit {name}");
        }
        Action::Edit {
            name,
            service,
            operation,
            schema,
        } => proposal::edit(
            &root,
            &name,
            service.as_deref(),
            operation.as_deref(),
            schema.as_deref(),
        )?,
        Action::Close { name } => {
            proposal::close(&root, &name)?;
            println!("Closed proposal {name}");
        }
        Action::Proposals { json: as_json } => {
            let proposals = proposal::list(&root)?;
            if as_json {
                print_json(&proposals)?;
            } else {
                for p in proposals {
                    println!("{} {}", p.name, if p.closed { "closed" } else { "open" });
                }
            }
        }
        Action::Hooks { command } => match command {
            HookCommand::Install => println!("Installed {}", git::install(&root)?.display()),
            HookCommand::Uninstall => {
                git::uninstall(&root)?;
                println!("Removed Docket hook");
            }
        },
        _ => unreachable!(),
    }
    Ok(0)
}

fn init(root: &Path, args: Init) -> Result<()> {
    util::safe_name(&args.service)?;
    let _lock = store::lock(root)?;
    let mut config = if root.join(".docket/config.toml").exists() {
        Config::load(root)?
    } else {
        Config::new(args.history.unwrap_or(HistoryMode::Full))
    };
    ensure!(
        !config
            .services
            .keys()
            .any(|name| name.eq_ignore_ascii_case(&args.service)),
        "service {} already exists; edit config.toml to change it",
        args.service
    );
    if let Some(mode) = args.history {
        config.history = mode;
    }
    let source = if let Some(path) = args.file {
        Source::File { path }
    } else if let Some(command) = args.command {
        let words = shell_words::split(&command)?;
        ensure!(!words.is_empty(), "export command is empty");
        Source::Command {
            command: words,
            cwd: args.cwd,
        }
    } else {
        let app = args
            .fastapi
            .context("provide --file, --command, or --fastapi")?;
        ensure!(
            app.contains(':'),
            "FastAPI app must have the form package.module:app"
        );
        Source::Fastapi {
            app,
            python: args.python,
            factory: args.factory,
            cwd: args.cwd,
            import_paths: if args.import_path.is_empty() {
                vec![".".into()]
            } else {
                args.import_path
            },
        }
    };
    for pattern in &args.input {
        globset::Glob::new(pattern)?;
    }
    config.services.insert(
        args.service.clone(),
        Service {
            source,
            inputs: args.input,
        },
    );
    config.save(root)?;
    for (file, bytes) in [
        (".gitignore", "local/\n"),
        (
            ".gitattributes",
            "head/** -text -filter\nhistory/** -text -filter\nobjects/** -text -filter\n",
        ),
    ] {
        let path = root.join(".docket").join(file);
        if !path.exists() {
            util::write_atomic(&path, bytes.as_bytes())?;
        }
    }
    println!("Registered {}; run docket sync", args.service);
    Ok(())
}

fn select(root: &Path, config: &Config, selector: &str) -> Result<Documents> {
    if selector == "working" {
        Ok(extract::extract(root, config, None)?.0)
    } else {
        store::baseline(root, selector)
    }
}
fn service_name(documents: &Documents, name: Option<&str>) -> Result<String> {
    if let Some(name) = name {
        ensure!(documents.contains_key(name), "unknown service {name}");
        return Ok(name.into());
    }
    ensure!(
        documents.len() == 1,
        "select --service when there is not exactly one service"
    );
    Ok(documents.keys().next().context("no contracts")?.clone())
}
fn print_json(value: &impl serde::Serialize) -> Result<()> {
    print!("{}", String::from_utf8(util::json_bytes(value)?)?);
    Ok(())
}
fn emit(root: &Path, path: Option<PathBuf>, text: String) -> Result<()> {
    if let Some(path) = path {
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        util::write_atomic(&path, text.as_bytes())?;
        eprintln!("Wrote {}", path.display());
    } else {
        print!("{text}");
    }
    Ok(())
}

fn watch(root: &Path, interval_ms: u64, debounce_ms: u64) -> Result<i32> {
    let mut fingerprint = String::new();
    let mut pending = Some(Instant::now());
    eprintln!("Watching contract inputs; Ctrl-C to stop");
    loop {
        let config = Config::load(root)?;
        let mut builder = globset::GlobSetBuilder::new();
        let all = config.services.values().any(|s| s.inputs.is_empty());
        for service in config.services.values() {
            for pattern in &service.inputs {
                builder.add(globset::Glob::new(pattern)?);
            }
        }
        let matcher = builder.build()?;
        let mut parts = Vec::new();
        parts.push(fs::read(root.join(".docket/config.toml"))?);
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                ![
                    ".git",
                    ".docket",
                    ".venv",
                    "venv",
                    "node_modules",
                    "target",
                    "__pycache__",
                ]
                .contains(&e.file_name().to_string_lossy().as_ref())
            })
        {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry.path().strip_prefix(root)?;
            if all || matcher.is_match(relative) {
                parts.push(relative.to_string_lossy().as_bytes().to_vec());
                parts.push(fs::read(entry.path())?);
            }
        }
        let current = util::hash(&parts.concat());
        if current != fingerprint {
            fingerprint = current;
            pending = Some(Instant::now());
        }
        if pending.is_some_and(|t| t.elapsed() >= Duration::from_millis(debounce_ms)) {
            let result = (|| -> Result<()> {
                let _lock = store::lock(root)?;
                store::verify(root)?;
                let (documents, _) = extract::extract(root, &config, None)?;
                let result = store::sync(root, &config, &documents)?;
                eprintln!(
                    "docket: {} {}",
                    if result.changed { "saved" } else { "unchanged" },
                    &result.snapshot[..12]
                );
                Ok(())
            })();
            if let Err(error) = result {
                eprintln!("docket: {error:#}");
            }
            pending = None;
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}
