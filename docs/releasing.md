# Binary distribution and releases

Docket's end-user packages contain native binaries. Rust and a C toolchain are
needed by maintainers building a release, never by someone installing it.
Every package includes the pinned oasdiff engine and both projects' licenses.
No npm install scripts, Rust build backends, or source distributions are used.

## Supported platforms

| Platform | CPU | Native Rust target | Packages |
|---|---|---|---|
| macOS 13+ | Apple Silicon | aarch64-apple-darwin | Homebrew, uv, npm, tar.gz |
| macOS 13+ | Intel | x86_64-apple-darwin | Homebrew, uv, npm, tar.gz |
| Linux | x86_64 | x86_64-unknown-linux-musl | .deb, Homebrew, uv, npm, tar.gz |
| Linux | ARM64 | aarch64-unknown-linux-musl | .deb, Homebrew, uv, npm, tar.gz |

Linux binaries are static. The packager rejects an ELF with a dynamic loader
or shared-library dependencies, including for the companion engine. Linux wheels
advertise glibc 2.17+ and musl 1.2+. This release pipeline does not package Windows
binaries. The Rust CLI can still be built from source on other supported targets.
FastAPI extraction always requires the user's own Python application environment.

## Local packaging and verification

Use Python 3.11+ for the packaging scripts; Python 3.9+ can run installed wheels.
The package and engine versions come from Cargo.toml and src/engine.rs. Registry
names and the release repository come from packaging/config.toml.

For example, on Apple Silicon:

```sh
cargo build --release --locked
engine="$(target/release/docket engine install)"
python3 scripts/distribute.py build \
  --target aarch64-apple-darwin \
  --binary target/release/docket --engine "$engine"
python3 scripts/distribute.py assemble --allow-partial --npm-local
python3 -m unittest discover -s tests -p 'test_distribution.py' -v
python3 scripts/smoke_packages.py --target aarch64-apple-darwin
bash scripts/smoke_brew.sh dist/local-Formula/docket.rb
```

The smoke test requires uv, npm and Node. It uses temporary tool directories,
removes Rust from the launchers' PATH, installs both packages, verifies the bundled
engine, checks exit codes, and generates a real contract report through each CLI.
The Homebrew test uses a temporary tap and removes its installation afterwards;
it refuses to run if Docket is already installed by Homebrew.

Replace the target and binary path for another native platform. On Linux,
install musl-tools, add the corresponding rustup target, set that target's Cargo
linker and C compiler to musl-gcc, and build with --target. The release workflow
shows the exact environment variables. Cross-target packages are not generated
by relabeling a host binary: architecture and native version checks must pass.

`dist/` is ignored by Git. It contains platform archives, wheels, npm platform
packages, `.deb` files on Linux, per-target manifests, and (after assembly):

- `docket-npm-VERSION.tgz`: launcher whose optional platform dependencies use
  fixed GitHub release URLs. Installable without publishing to npm.
- `docket-npm-registry-VERSION.tgz`: launcher with exact npm registry dependencies.
- `docket.rb`: Homebrew formula with per-platform URLs and real SHA-256 hashes.
- `docket_release-VERSION-py3-none-any.whl`: universal uv installer. Standard
  Python dependency markers select a native wheel at a versioned, SHA-256-pinned
  release URL. The tool environment is named `docket-release`; the command is
  `docket`. This selector is distributed on GitHub, while the native `docket`
  wheels are the packages published to PyPI.
- `SHA256SUMS`: hashes of all release downloads.

`--npm-local` additionally creates a local-only npm launcher and a formula
referencing local files for tests, plus a local uv selector wheel. These are never uploaded by the release workflow.
Normal assembly requires all four targets. `--allow-partial` is for local tests.
Archive ownership, timestamps and file ordering are fixed for repeatable packaging
of the same input bytes. Compiling Rust itself is not claimed to be reproducible.

## Publish a GitHub release

1. Update Cargo.toml/Cargo.lock and the installation examples to the release
   version. Change src/engine.rs only when deliberately updating the engine pin
   and its upstream checksums. Keep scripts/OASDIFF-LICENSE current with that pin.
2. Merge the reviewed changes into main and push a matching tag, e.g. `v0.1.0`.
3. The `binary release` workflow builds all four native targets, runs core tests,
   verifies package formats, and installs with uv/npm on every target, apt-get on
   Linux and Homebrew on macOS. Only then does it publish the GitHub release.
4. It commits the generated `Formula/docket.rb` to main, making this repository
   a Homebrew tap. Enable workflow write access to repository contents. If branch
   protection blocks that push, copy the release's `docket.rb` asset into
   `Formula/docket.rb` in a normal reviewed change; the binary release remains
   available. Old tag reruns cannot downgrade the formula.

The workflow also accepts a manual run with an existing version tag selected.
No registry account or credentials are needed for GitHub-hosted uv/npm installers.
Use immutable release versions; replacing existing assets can break cached hashes.
Tags without a matching Cargo version are rejected. Stable x.y.z releases are
supported by the packaging workflows.

## Optional short PyPI and npm commands

After registry publication, users can install with:

```sh
uv tool install docket
npm install --global @gyraseisfortorsion/docket
```

These commands require publishing to the respective registries first. The npm
names `docket` and `docket-cli` belong to unrelated packages; the installed
executable is still named `docket` for our scoped package.

For PyPI, reserve the `docket` project with a pending trusted publisher for this
GitHub repository, workflow `publish-registries.yml`, environment `pypi`. Enable
that environment in GitHub and configure any required reviewers. For npm, grant
the publisher access to `@gyraseisfortorsion`, create an appropriate publishing
token, and add it as `NPM_TOKEN` in the GitHub `npm` environment. Do not commit
credentials. If registry names become unavailable, update packaging/config.toml
and the docs before producing release artifacts.

Run `publish package registries` manually, choose the existing release tag,
and select PyPI, npm, or both. It downloads/checksums the already-tested release
packages, publishes all native npm dependencies before the root launcher, and
publishes only native `docket` wheels to PyPI (the GitHub selector is excluded).
The npm launcher and native packages use exact matching versions. npm needs optional dependencies enabled; Node 18+ is
required by its launcher. uv manages a Python runtime, but needs no Rust.

The apt installation path uses downloaded Debian packages:
`sudo apt-get install ./docket_VERSION_ARCH.deb`. It does not configure an apt
repository, so updates require downloading a new package. Supporting
`apt-get install docket` and automatic apt upgrades would additionally require
an operated, signed apt repository and signing-key distribution.

Git hooks store the CLI's absolute native path. Rerun `docket hooks install` in
your repositories after upgrading or switching installers if that path changes
(for example, after a Homebrew upgrade removes an older Cellar version).
