# Docket

Docket is a Rust CLI for local version control of OpenAPI contracts. It extracts provider
schemas, keeps browsable endpoint/model views in `.docket`, records portable
history, and explains API changes to people and coding agents.

Supported inputs: **OpenAPI 3.0.x / 3.1.x JSON**, JSON export commands, and FastAPI
apps or factories. A pinned **oasdiff 1.34.1** companion performs compatibility
analysis. Everything operates locally; comparisons do not upload contracts.

## Install and try it

Build with Rust 1.85 or newer and a C compiler for the compression dependency:

```sh
cargo install --path . --locked
docket engine install
```

The engine installer requires `curl`, verifies a pinned SHA-256 checksum, and
installs into `$XDG_CACHE_HOME/docket` or `~/.cache/docket`. It supports
macOS universal, Linux x86_64/arm64, and Windows x86_64/arm64 engine releases.
The automated integration suite runs on macOS and Linux.

In an API repository:

```sh
docket init --service users-api --file openapi.json
docket sync
docket status
docket show --operation 'POST /users'
docket report --format html --output docket.html
```

`init` registers configuration; `sync` captures the first snapshot. Run `init`
again with a different service name to register another provider. For a quick
example, copy `examples/users.openapi.json` into an empty directory as
`openapi.json` and use the commands above.

`docket --root /path/to/project ...` selects a project explicitly. Otherwise,
commands search upward for `.docket/config.toml`.

## FastAPI and other frameworks

```sh
docket init --service users-api \
  --fastapi app.main:app --python .venv/bin/python
docket sync --timings
```

The configured interpreter must already contain your application dependencies.
The adapter imports the app and calls `app.openapi()`. It does not start a server
or execute lifespan handlers. Import-time effects still execute. For applications
with expensive imports or environment requirements, provide an import-safe factory:

```sh
docket init --service users-api \
  --fastapi app.schema:create_app --factory \
  --python .venv/bin/python --import-path src
```

`--cwd` sets the extraction working directory, relative to the project.
`--import-path` is repeatable and relative to the project, not `--cwd`.
The project root is the default import path. Import and generation timings are
reported separately from normalization and storage.

For any other framework, export OpenAPI JSON to stdout:

```sh
docket init --service payments-api --command 'node scripts/export-openapi.js'
```

Command strings are parsed into arguments **without invoking a shell**. Use a
script for pipelines. Output must be JSON only; diagnostics belong on stderr.
Commands receive `DOCKET_SOURCE_ROOT` and, during hooks,
`DOCKET_ORIGINAL_ROOT`. Custom exporters are responsible for respecting the
staged working directory and avoiding imports of unstaged application source.

For file inputs, local JSON `$ref` documents are bundled into the snapshot.
Recursive references stay references rather than expanding forever. External
documents must be inside the project root. Remote references and YAML inputs
are not supported; bundle/convert them before ingestion. Supplied examples are
preserved; sample payloads are not invented.

## Storage and history

```text
.docket/
  config.toml
  .gitignore
  .gitattributes
  head/
    manifest.json
    users-api/
      openapi.json
      index.json
      operations/<tag>--<hash>/<operation>--<hash>.json
      schemas/UserCreate.json
  proposals/<name>/
    proposal.json
    base/<service>/openapi.json
    <service>/openapi.json
  history/<revision>.json
  objects/<content-hash>.json.zst
  local/                         # ignored locks, staging, recovered edits
```

The complete `openapi.json` is authoritative for each observed service. Operation
files are navigation views containing the operation, effective parameters/auth,
servers, and transitive referenced definitions. Their `source` names the full
document against which references resolve; these views are not standalone
OpenAPI documents. `index.json` maps identities and original schema names to
filenames.

Operation identity is **service + method + exact path**. Tags and operation IDs
affect display paths, but do not turn an endpoint into a different endpoint.
Tag and operation suffixes prevent case-insensitive filename collisions.

Object keys and whitespace normalize deterministically. Array order is
preserved. Documentation and examples remain part of history. Unchanged syncs
do not rewrite generated files or create revisions. Each command validates
reference integrity and essential OpenAPI structure; compatibility analysis
adds the engine's schema/compatibility checks. This is not runtime verification.

Choose storage at initialization:

```sh
docket init --service users-api --file openapi.json --history full
# or --history head
```

- `full` (default): immutable revision records and deduplicated, compressed full
  documents; copy `.docket` to retain history without the source tree or Git.
- `head`: current contracts and active proposal bases; no new archive is created.
  Git can still supply earlier versions of committed contracts.

Changing `history` in configuration affects subsequent syncs. Switching to
`head` does not delete an existing archive. Full-mode revision parents describe
snapshot ancestry, not Git commits; canceled commits can leave valid uncommitted
snapshots. Archive records from different Git branches can coexist.

```sh
docket log
docket show --at REVISION --operation 'GET /users/{id}'
docket diff --from previous --to head
docket diff --from REVISION --to working --format json
docket diff --from git:origin/main --to head
```

`head`/`current` means the saved snapshot. `working` performs fresh extraction.
Revision prefixes need at least six hexadecimal characters. `previous` selects
the saved revision's native parent. Git baselines read saved `.docket/head`
documents from the selected commit; they do not execute historical source code.

Generated files are protected against manual modifications/deletions. Restore
them from Git or a saved archive before syncing; use proposals for desired API
changes. After a Git merge conflicts in generated head, resolve the source and
restore one complete side of generated head before syncing. Preserve immutable
history/object files from both sides. There is no independent branch/merge system.

## Propose and edit an API change

```sh
docket propose add-preferences
EDITOR=vim docket edit add-preferences --schema UserCreate
docket edit add-preferences --operation 'POST /users'
docket diff --proposal add-preferences --format markdown
docket proposals
docket close add-preferences
```

Set `EDITOR` or `VISUAL`; `VISUAL` takes precedence. For multiple providers,
select `--service`. Omitting both `--schema` and `--operation` edits the complete
desired document. You can also edit the proposal's `<service>/openapi.json`
directly.

Edits validate before replacing the proposal. Invalid targeted edits leave the
proposal unchanged and retain a recovery file under `.docket/local`.
Proposals retain their base even in head-only mode. Sync never overwrites them.
Closing is explicit, preserves files, and removes the proposal from open drift
reports. No application code is generated or changed.

Proposal drift compares the complete desired service snapshot to fresh observed
contracts. Unrelated API changes can therefore appear in the drift; automatic
three-way proposal rebasing is not part of v1.

## Automatic updates on Git commits

First stage configuration and generated contracts, then install the hook:

```sh
git add .docket
docket hooks install
git add app/
git commit -m 'Update users API'
```

Installation preserves an existing executable pre-commit hook by chaining it
before Docket, honors `core.hooksPath`, and changes no Git hook configuration.
The wrapper records this installation's executable and project paths. Reinstall
after moving the checkout or executable. `docket hooks uninstall` restores
the original hook.

The hook reads **indexed configuration**, exports the index to a temporary tree,
extracts staged schemas, and stages only generated head/history/object artifacts.
Unstaged source and proposal edits remain untouched. FastAPI's dependency
environment is reused; repository-owned modules must load from the staged tree.
An editable installation pointing back to unstaged source produces an error;
set `import_paths` to include every repository source root.

Breaking changes print warnings and allow the commit. Invalid schemas, failed
extraction, unavailable/wrong engine versions, conflicts, or concurrent index
changes stop it. Unchanged contracts create no revision. Gitlinks/submodules are
not supported by staged-tree extraction in v1. Hooks are local and bypassable;
install them separately in each clone and enforce policy in CI.

By default, any staged input outside `.docket` can trigger extraction. This
is conservative because schema generation can depend on configuration or shared
code. Optional input globs narrow it:

```toml
format_version = 1
history = "full"
timeout_seconds = 120

[services.users-api]
source = "fastapi"
app = "app.main:app"
python = ".venv/bin/python"
factory = false
import_paths = ["src", "shared"]
inputs = ["src/**", "shared/**", "pyproject.toml", "uv.lock"]
```

Globs are relative to the project. Include all schema-affecting inputs. Changes
to indexed `config.toml` always trigger extraction. With generated schemas, the
tool must run the exporter to learn whether the contract changed.

## Reports, coding agents, and CI

```sh
docket diff --format json
docket report --since git:origin/main --format markdown --output changes.md
docket report --format txt --output docket.txt
docket report --format html --output docket.html
docket report --working --format json --output docket.json
docket check --against git:origin/main --fail-on breaking
```

Reports use one versioned model across Markdown, TXT, self-contained HTML, and
JSON. They include inventories, supplied examples, effective auth/parameters,
compatibility findings, complete structural/documentation changes, JSON pointers,
before/after values, transitive endpoint impact, open proposal drift, snapshot
hashes, and tool/engine versions. HTML escapes contract text and has no remote
assets or scripts. JSON gives coding agents the delta without requiring them to
infer changes from two large documents.

Compatibility findings classify `breaking`, `warning` (uncertain), or
`compatible` using the pinned engine. Complete JSON changes are kept separately:
an empty compatibility result never hides documentation edits.

`report` reads saved head by default so it works from a portable `.docket`
copy. `--working` extracts fresh source. Without `--since`, the report compares
saved head with itself and includes the complete inventory/proposal state.
`diff` defaults to saved head → fresh source.
Its output focuses on changes; use `report` when you also want the full inventory.

`check` always extracts fresh source and rejects a stale/missing saved head.
Its additional policy can fail on `breaking` (default), any `change`, or open
proposal `drift`. Exit codes: **0** success, **1** policy failure, **2** operational
or validation failure. Configure CI to fetch the requested Git baseline:

```yaml
- uses: actions/checkout@v4
  with:
    fetch-depth: 0
- run: docket check --against git:origin/main --fail-on breaking
```

All comparisons require exactly the pinned engine. The lookup order is
`DOCKET_OASDIFF`, beside the CLI executable, the managed cache, then `PATH`.
The explicit override is useful for offline/managed installations; version
validation still applies. Existing repository/user oasdiff config is ignored to
make analysis reproducible.

`docket watch` is an optional debounced polling watcher for development. It
excludes `.git`, `.docket`, environments, dependency/build directories, and
respects input globs. CI/hooks remain the source of commit-time correctness.

## Development and verification

```sh
cargo test --locked                         # core suite; no Python/network needed
cargo run --locked -- engine install
python3 -m venv .venv
.venv/bin/python -m pip install -r requirements-dev.txt
DOCKET_TEST_PYTHON="$PWD/.venv/bin/python" \
  cargo test --locked --all-targets -- --include-ignored
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
python3 scripts/benchmark.py --binary target/release/docket
# Optionally include fresh FastAPI imports and schema generation:
python3 scripts/benchmark.py --fastapi-python .venv/bin/python --sizes 100 1000
```

Real-engine/Python integration tests are opt-in so ordinary core tests do not
download software or require FastAPI. CI runs all of them on macOS and Linux,
including partial commits with editable source-path injection, compatibility
directionality, hook chaining, stale-snapshot checks, and invalid-edit recovery.
Benchmarks report extraction/normalization, storage, and compatibility costs
separately for 100, 1,000, and 10,000 endpoints.

`python3 scripts/demo.py` generates `target/demo-report.html` with a real breaking
change, the complete inventory, and an unfinished proposal.

`sh scripts/package.sh` builds a local distribution containing the CLI, pinned
engine, documentation, and both licenses. It performs no publishing.

Out of scope for v1: consumer discovery, runtime conformance tests, remote
registries, automatic documentation publishing, and application code generation.
