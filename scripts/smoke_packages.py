#!/usr/bin/env python3
"""Exercise real uv/npm installs in temporary prefixes, with Rust absent.

Run distribute.py build and assemble --allow-partial --npm-local first. Requires
Python 3.11+, uv, npm and node. Does not alter the user's installed tools.
"""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

SPEC = importlib.util.spec_from_file_location("distribute", Path(__file__).with_name("distribute.py"))
DIST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DIST)


def run(arguments, env, cwd=None, expected=0):
    result = subprocess.run([str(arg) for arg in arguments], env=env, cwd=cwd,
                            capture_output=True, text=True, timeout=180)
    if result.returncode != expected:
        raise AssertionError(f"{arguments}: expected {expected}, got {result.returncode}\n{result.stdout}\n{result.stderr}")
    return result.stdout.strip()


def verify(command, env, work, expected_binary_directory):
    assert run([command, "--version"], env) == f"docket {DIST.VERSION}"
    engine = Path(run([command, "engine", "path"], env))
    assert engine.parent.resolve() == expected_binary_directory.resolve(), (engine, expected_binary_directory)
    assert DIST.ENGINE_VERSION in run([engine, "--version"], env)
    run([command, "--definitely-invalid"], env, expected=2)
    work.mkdir()
    shutil.copy(DIST.ROOT / "examples/users.openapi.json", work / "openapi.json")
    run([command, "init", "--service", "users", "--file", "openapi.json"], env, work)
    run([command, "sync"], env, work)
    report = json.loads(run([command, "report", "--format", "json"], env, work))
    assert report, "installed package did not generate a report"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, default=DIST.ROOT / "dist")
    parser.add_argument("--target", choices=DIST.TARGETS, required=True)
    args = parser.parse_args()
    directory = args.directory.resolve()
    uv, npm, node = [shutil.which(name) for name in ("uv", "npm", "node")]
    if not all((uv, npm, node)):
        parser.error("uv, npm and node are needed for installation smoke tests")
    with tempfile.TemporaryDirectory(prefix="docket-package-test-") as temporary:
        root = Path(temporary)
        tools = root / "tools"
        tools.mkdir()
        # Include only the runtimes needed by the installers, never cargo/rustc.
        for name, executable in (("node", node), ("sh", "/bin/sh")):
            (tools / name).symlink_to(executable)
        env = {**os.environ, "PATH": str(tools), "HOME": str(root / "home"),
               "XDG_CACHE_HOME": str(root / "cache"), "UV_CACHE_DIR": str(root / "uv-cache"),
               "UV_TOOL_DIR": str(root / "uv-tools"), "UV_TOOL_BIN_DIR": str(root / "uv-bin"),
               "npm_config_cache": str(root / "npm-cache"), "npm_config_userconfig": str(root / "npmrc")}
        env.pop("DOCKET_OASDIFF", None)
        assert shutil.which("rustc", path=env["PATH"]) is None
        wheel = directory / "local" / f"docket_release-{DIST.VERSION}-py3-none-any.whl"
        run([uv, "tool", "install", "--no-index", "--python", __import__("sys").executable, wheel], env)
        uv_command = root / "uv-bin/docket"
        uv_binaries = root / "uv-tools/docket-release"
        installed = list(uv_binaries.glob("lib/python*/site-packages/docket_cli/bin"))
        assert len(installed) == 1, installed
        verify(uv_command, env, root / "uv-project", installed[0])
        prefix = root / "npm-prefix"
        run([npm, "install", "--global", "--prefix", prefix, "--ignore-scripts", "--no-audit", "--no-fund",
             directory / f"docket-npm-local-{DIST.VERSION}.tgz"], env)
        npm_command = prefix / "bin/docket"
        platform = DIST.TARGETS[args.target]["npm"]
        native = prefix / "lib/node_modules" / DIST.CONFIG["npm_scope"] / "docket/node_modules" / DIST.CONFIG["npm_scope"] / f"docket-{platform}/bin"
        verify(npm_command, env, root / "npm-project", native)
        print("uv and npm: isolated installs, bundled engine, exit codes and real contract reports passed without Rust")


if __name__ == "__main__":
    main()
