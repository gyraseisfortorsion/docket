#!/usr/bin/env python3
"""Build binary-only distributions using Python 3.11+ and the standard library.

No installer compiles Rust or runs a custom binary downloader. Build
with a native target's verified docket/oasdiff pair, then assemble all targets.
"""

import argparse
import base64
import csv
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import tarfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
CONFIG = tomllib.loads((ROOT / "packaging/config.toml").read_text())
CARGO = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
VERSION = CARGO["version"]
if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", VERSION):
    raise ValueError("binary packaging currently supports stable x.y.z versions")
ENGINE_VERSION = re.search(r'pub const VERSION: &str = "([^"]+)";',
                           (ROOT / "src/engine.rs").read_text()).group(1)
BASE = f"https://github.com/{CONFIG['repository']}/releases/download/v{VERSION}"
# Linux binaries must be static, including oasdiff. That lets the same wheel
# run on glibc >=2.17 and musl >=1.2 without a host-specific shared runtime.
TARGETS = {
    "aarch64-apple-darwin": {"npm": "darwin-arm64", "wheel": "macosx_13_0_arm64", "arch": "arm64"},
    "x86_64-apple-darwin": {"npm": "darwin-x64", "wheel": "macosx_13_0_x86_64", "arch": "amd64"},
    "aarch64-unknown-linux-musl": {"npm": "linux-arm64", "wheel": "manylinux_2_17_aarch64.musllinux_1_2_aarch64", "arch": "arm64"},
    "x86_64-unknown-linux-musl": {"npm": "linux-x64", "wheel": "manylinux_2_17_x86_64.musllinux_1_2_x86_64", "arch": "amd64"},
}
EPOCH = max(315532800, int(os.environ.get("SOURCE_DATE_EPOCH", "315532800")))


def digest(data):
    return hashlib.sha256(data).hexdigest()


def json_bytes(value):
    return (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()


def tar_bytes(files):
    """files maps POSIX paths to (bytes, mode); no host uid or timestamps."""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as archive:
        directories = sorted({str(parent) for name in files for parent in Path(name).parents
                              if str(parent) != "."})
        for name in directories:
            item = tarfile.TarInfo(name + "/")
            item.mode, item.mtime, item.type = 0o755, EPOCH, tarfile.DIRTYPE
            archive.addfile(item)
        for name, (data, mode) in sorted(files.items()):
            item = tarfile.TarInfo(name)
            item.size, item.mode, item.mtime = len(data), mode, EPOCH
            archive.addfile(item, io.BytesIO(data))
    return gzip.compress(raw.getvalue(), mtime=EPOCH)


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return path


def validate_binary(data, target):
    """Reject accidentally packaging a host binary under a different target."""
    expected = TARGETS[target]["arch"]
    if "linux" in target:
        if data[:6] != b"\x7fELF\x02\x01":
            raise ValueError(f"{target} requires a little-endian ELF64 executable")
        machine = struct.unpack_from("<H", data, 18)[0]
        if machine != {"amd64": 62, "arm64": 183}[expected]:
            raise ValueError(f"ELF architecture does not match {target}")
        phoff = struct.unpack_from("<Q", data, 32)[0]
        phsize, phnum = struct.unpack_from("<HH", data, 54)
        if phsize < 56 or phoff + phsize * phnum > len(data):
            raise ValueError("invalid ELF program headers")
        for i in range(phnum):
            offset = phoff + i * phsize
            kind = struct.unpack_from("<I", data, offset)[0]
            if kind == 3:
                raise ValueError("Linux packages require static binaries (no dynamic loader)")
            if kind == 2:
                start = struct.unpack_from("<Q", data, offset + 8)[0]
                size = struct.unpack_from("<Q", data, offset + 32)[0]
                if start + size > len(data) or size % 16:
                    raise ValueError("invalid ELF dynamic section")
                if any(struct.unpack_from("<Q", data, pos)[0] == 1
                       for pos in range(start, start + size, 16)):
                    raise ValueError("Linux packages require static binaries (no shared dependencies)")
    else:
        cpu = {"amd64": 0x01000007, "arm64": 0x0100000C}[expected]
        if data[:4] == b"\xcf\xfa\xed\xfe":
            available = [struct.unpack_from("<I", data, 4)[0]]
        elif data[:4] == b"\xca\xfe\xba\xbe":
            count = struct.unpack_from(">I", data, 4)[0]
            available = [struct.unpack_from(">I", data, 8 + 20 * i)[0] for i in range(count)]
        else:
            raise ValueError(f"{target} requires a Mach-O executable")
        if cpu not in available:
            raise ValueError(f"Mach-O architecture does not match {target}")


def common_files():
    return {
        "LICENSE": ((ROOT / "LICENSE").read_bytes(), 0o644),
        "OASDIFF-LICENSE": ((ROOT / "scripts/OASDIFF-LICENSE").read_bytes(), 0o644),
        "README.md": ((ROOT / "README.md").read_bytes(), 0o644),
        "VERSIONS.json": (json_bytes({"docket": VERSION, "oasdiff": ENGINE_VERSION}), 0o644),
    }


def build_wheel(output, target, files):
    distribution = CONFIG["python_distribution"].replace("-", "_")
    info = f"{distribution}-{VERSION}.dist-info"
    tag = f"py3-none-{TARGETS[target]['wheel']}"
    contents = {}
    for source in sorted((ROOT / "packaging/python/docket_cli").glob("*.py")):
        contents[f"docket_cli/{source.name}"] = (source.read_bytes(), 0o644)
    for name in ("docket", "oasdiff"):
        contents[f"docket_cli/bin/{name}"] = files[name]
    for name in ("LICENSE", "OASDIFF-LICENSE"):
        contents[f"{info}/licenses/{name}"] = files[name]
    contents[f"{info}/METADATA"] = ((
        f"Metadata-Version: 2.4\nName: {CONFIG['python_distribution']}\nVersion: {VERSION}\n"
        f"Summary: {CARGO['description']}\nDescription-Content-Type: text/plain\nRequires-Python: >=3.9\n"
        "License-Expression: MIT AND Apache-2.0\n"
        "License-File: LICENSE\nLicense-File: OASDIFF-LICENSE\n"
        f"Project-URL: Source, https://github.com/{CONFIG['repository']}\n\n"
        "Docket CLI and pinned oasdiff engine, prebuilt for this platform.\n"
    ).encode(), 0o644)
    tags = "".join(f"Tag: py3-none-{platform}\n" for platform in TARGETS[target]["wheel"].split("."))
    contents[f"{info}/WHEEL"] = ((
        "Wheel-Version: 1.0\nGenerator: docket-distribute\nRoot-Is-Purelib: false\n" + tags
    ).encode(), 0o644)
    contents[f"{info}/entry_points.txt"] = (b"[console_scripts]\ndocket = docket_cli:main\n", 0o644)
    return write_wheel(output, distribution, tag, contents)


def write_wheel(output, distribution, tag, contents):
    info = f"{distribution}-{VERSION}.dist-info"
    records = io.StringIO(newline="")
    writer = csv.writer(records, lineterminator="\n")
    for name, (data, _) in sorted(contents.items()):
        encoded = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
        writer.writerow([name, "sha256=" + encoded, len(data)])
    writer.writerow([f"{info}/RECORD", "", ""])
    contents[f"{info}/RECORD"] = (records.getvalue().encode(), 0o644)
    path = output / f"{distribution}-{VERSION}-{tag}.whl"
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, (data, mode) in sorted(contents.items()):
            item = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
            item.create_system = 3
            item.external_attr = (0o100000 | mode) << 16
            item.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(item, data)
    return path


def build_uv_installer(output, manifests, local=False):
    # A universal wheel lets one release URL work on all supported platforms.
    # uv installs the matching native wheel through standard PEP 508 markers;
    # no dynamic downloader, build backend, or hosted Python index is involved.
    info = f"docket_release-{VERSION}.dist-info"
    requirements = []
    for target, manifest in sorted(manifests.items()):
        artifact = manifest["artifacts"]["wheel"]
        filename = artifact["file"]
        url = (output / filename).as_uri() if local else f"{BASE}/{filename}"
        system = TARGETS[target]["npm"].split("-")[0]
        machine = {"darwin-arm64": "arm64", "linux-arm64": "aarch64"}.get(
            TARGETS[target]["npm"], "x86_64")
        marker = f'sys_platform == "{system}" and platform_machine == "{machine}"'
        requirements.append(f"Requires-Dist: {CONFIG['python_distribution']} @ {url}#sha256={artifact['sha256']} ; {marker}\n")
    metadata = (
        f"Metadata-Version: 2.4\nName: docket-release\nVersion: {VERSION}\n"
        "Summary: Platform selector for the Docket GitHub release\nDescription-Content-Type: text/plain\nRequires-Python: >=3.9\n"
        "License-Expression: MIT\nLicense-File: LICENSE\n" + "".join(requirements) + "\n"
        "Install the native Docket release for this interpreter's platform.\n"
    )
    contents = {
        f"{info}/METADATA": (metadata.encode(), 0o644),
        f"{info}/WHEEL": (b"Wheel-Version: 1.0\nGenerator: docket-distribute\nRoot-Is-Purelib: true\nTag: py3-none-any\n", 0o644),
        f"{info}/licenses/LICENSE": ((ROOT / "LICENSE").read_bytes(), 0o644),
        f"{info}/entry_points.txt": (b"[console_scripts]\ndocket = docket_release:main\n", 0o644),
        "docket_release/__init__.py": ((ROOT / "packaging/python/docket_release/__init__.py").read_bytes(), 0o644),
    }
    destination = output / "local" if local else output
    return write_wheel(destination, "docket_release", "py3-none-any", contents)


def npm_metadata(name):
    return {"name": name, "version": VERSION, "description": CARGO["description"],
            "license": "MIT AND Apache-2.0", "repository": {
                "type": "git", "url": f"git+https://github.com/{CONFIG['repository']}.git"},
            "homepage": f"https://github.com/{CONFIG['repository']}",
            "publishConfig": {"access": "public"}}


def build_npm_platform(output, target, files):
    platform = TARGETS[target]["npm"]
    os_name, cpu = platform.split("-")
    metadata = npm_metadata(f"{CONFIG['npm_scope']}/docket-{platform}")
    metadata.update({"os": [os_name], "cpu": [cpu]})
    contents = {f"package/{name}": value for name, value in files.items() if name not in ("docket", "oasdiff")}
    for name in ("docket", "oasdiff"):
        contents[f"package/bin/{name}"] = files[name]
    contents["package/package.json"] = (json_bytes(metadata), 0o644)
    return write(output / f"docket-npm-{platform}-{VERSION}.tgz", tar_bytes(contents))


def ar_member(name, data):
    # System V ar headers, the .deb container format. Names here are <=16 bytes.
    header = f"{name + '/':<16}{EPOCH:<12}{0:<6}{0:<6}{'100644':<8}{len(data):<10}`\n".encode("ascii")
    assert len(header) == 60
    return header + data + (b"\n" if len(data) % 2 else b"")


def build_deb(output, target, files):
    arch = TARGETS[target]["arch"]
    payload = {
        "usr/lib/docket/docket": files["docket"],
        "usr/lib/docket/oasdiff": files["oasdiff"],
        "usr/bin/docket": (b'#!/bin/sh\nexec /usr/lib/docket/docket "$@"\n', 0o755),
    }
    for name in ("LICENSE", "OASDIFF-LICENSE", "README.md", "VERSIONS.json"):
        payload[f"usr/share/doc/docket/{name}"] = files[name]
    payload["usr/share/doc/docket/copyright"] = (
        b"Docket: MIT; bundled oasdiff: Apache-2.0. See LICENSE and OASDIFF-LICENSE.\n", 0o644)
    size = sum(len(data) for data, _ in payload.values())
    control = (
        f"Package: docket\nVersion: {VERSION}\nSection: devel\nPriority: optional\n"
        f"Architecture: {arch}\nInstalled-Size: {(size + 1023) // 1024}\n"
        f"Maintainer: Docket contributors <noreply@github.com>\n"
        f"Homepage: https://github.com/{CONFIG['repository']}\n"
        "Description: Local version control for OpenAPI contracts\n"
        " Includes the native Docket CLI and its pinned compatibility engine.\n"
    ).encode()
    contents = b"!<arch>\n" + ar_member("debian-binary", b"2.0\n")
    contents += ar_member("control.tar.gz", tar_bytes({"control": (control, 0o644)}))
    contents += ar_member("data.tar.gz", tar_bytes(payload))
    return write(output / f"docket_{VERSION}_{arch}.deb", contents)


def build(args):
    target, output = args.target, args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    files = common_files()
    for name, source in (("docket", args.binary), ("oasdiff", args.engine)):
        data = source.read_bytes()
        validate_binary(data, target)
        version_output = subprocess.run([str(source.resolve()), "--version"], check=True,
                                        capture_output=True, text=True, timeout=15).stdout
        expected = VERSION if name == "docket" else ENGINE_VERSION
        if expected not in [word.removeprefix("v") for word in version_output.split()]:
            raise ValueError(f"{name} version does not match {expected}: {version_output.strip()}")
        files[name] = (data, 0o755)
    artifacts = {
        "archive": write(output / f"docket-{VERSION}-{target}.tar.gz", tar_bytes(files)),
        "wheel": build_wheel(output, target, files),
        "npm": build_npm_platform(output, target, files),
    }
    if "linux" in target:
        artifacts["deb"] = build_deb(output, target, files)
    manifest = {"target": target, "version": VERSION, "engine_version": ENGINE_VERSION,
                "artifacts": {kind: {"file": path.name, "sha256": digest(path.read_bytes())}
                              for kind, path in artifacts.items()}}
    write(output / f"manifest-{target}.json", json_bytes(manifest))
    for path in artifacts.values():
        print(path)


def build_npm_root(output, manifests, kind):
    metadata = npm_metadata(f"{CONFIG['npm_scope']}/docket")
    metadata.update({"bin": {"docket": "bin/docket.cjs"}, "engines": {"node": ">=18"},
                     "optionalDependencies": {}})
    for target, manifest in sorted(manifests.items()):
        platform = TARGETS[target]["npm"]
        filename = manifest["artifacts"]["npm"]["file"]
        dependency = {"registry": VERSION, "release": f"{BASE}/{filename}",
                      "local": (output / filename).as_uri()}[kind]
        metadata["optionalDependencies"][f"{CONFIG['npm_scope']}/docket-{platform}"] = dependency
    contents = {f"package/{name}": value for name, value in common_files().items()}
    contents["package/bin/docket.cjs"] = ((ROOT / "packaging/npm/docket.cjs").read_bytes(), 0o755)
    contents["package/package.json"] = (json_bytes(metadata), 0o644)
    suffix = "" if kind == "release" else f"-{kind}"
    return write(output / f"docket-npm{suffix}-{VERSION}.tgz", tar_bytes(contents))


def formula(manifests, local=False, directory=None):
    lines = ["# Generated by scripts/distribute.py; checksums are from release artifacts.",
             "class Docket < Formula", f'  desc "{CARGO["description"]}"',
             f'  homepage "https://github.com/{CONFIG["repository"]}"',
             f'  version "{VERSION}"', '  license all_of: ["MIT", "Apache-2.0"]', ""]
    for operating_system in ("darwin", "linux"):
        matches = [(target, manifest) for target, manifest in sorted(manifests.items()) if operating_system in target]
        if not matches:
            continue
        lines.append(f"  on_{'macos' if operating_system == 'darwin' else 'linux'} do")
        for target, manifest in matches:
            archive = manifest["artifacts"]["archive"]
            url = (directory / archive["file"]).as_uri() if local else f"{BASE}/{archive['file']}"
            lines += [f"    on_{'arm' if TARGETS[target]['arch'] == 'arm64' else 'intel'} do",
                      f'      url "{url}"', f'      sha256 "{archive["sha256"]}"', "    end"]
        lines += ["  end", ""]
    if any("darwin" in target for target in manifests):
        lines.append('  depends_on macos: :ventura if OS.mac?')
    lines += ["", "  def install", '    libexec.install "docket", "oasdiff"',
              '    bin.write_exec_script libexec/"docket"',
              '    doc.install "VERSIONS.json", "OASDIFF-LICENSE"', "  end", "",
              "  test do", f'    assert_match "docket {VERSION}", shell_output("#{{bin}}/docket --version")',
              '    assert_equal "#{libexec}/oasdiff", shell_output("#{bin}/docket engine path").strip',
              "  end", "end", ""]
    return "\n".join(lines).encode()


def assemble(args):
    source, output = args.input.resolve(), args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    manifests = {}
    artifacts = set()
    for path in sorted(source.glob("manifest-*.json")):
        manifest = json.loads(path.read_text())
        target = manifest["target"]
        if target not in TARGETS or target in manifests:
            raise ValueError(f"unsupported or duplicate target: {target}")
        if manifest["version"] != VERSION or manifest["engine_version"] != ENGINE_VERSION:
            raise ValueError(f"version mismatch in {path.name}")
        expected = {"archive", "wheel", "npm"} | ({"deb"} if "linux" in target else set())
        if set(manifest["artifacts"]) != expected:
            raise ValueError(f"missing or unexpected artifact types in {path.name}")
        for artifact in manifest["artifacts"].values():
            name = artifact["file"]
            if Path(name).name != name or name in artifacts:
                raise ValueError(f"unsafe or duplicate artifact: {name}")
            data = (source / name).read_bytes()
            if digest(data) != artifact["sha256"]:
                raise ValueError(f"checksum mismatch: {name}")
            if source != output:
                write(output / name, data)
            artifacts.add(name)
        manifests[target] = manifest
    if not manifests or (not args.allow_partial and set(manifests) != set(TARGETS)):
        raise ValueError("expected all four target manifests; use --allow-partial for local testing")
    for kind in ("release", "registry") + (("local",) if args.npm_local else ()):
        artifacts.add(build_npm_root(output, manifests, kind).name)
    formula_path = write(output / "docket.rb", formula(manifests))
    artifacts.add(str(formula_path.relative_to(output)))
    if args.npm_local:
        write(output / "local-Formula/docket.rb", formula(manifests, local=True, directory=output))
    artifacts.add(build_uv_installer(output, manifests).name)
    if args.npm_local:
        build_uv_installer(output, manifests, local=True)
    checksums = "".join(f"{digest((output / name).read_bytes())}  {name}\n" for name in sorted(artifacts))
    write(output / "SHA256SUMS", checksums.encode())
    print(f"Assembled Docket {VERSION} for {len(manifests)} platforms in {output}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build_parser = commands.add_parser("build", help="package an already-built native target")
    build_parser.add_argument("--target", choices=TARGETS, required=True)
    build_parser.add_argument("--binary", type=Path, required=True)
    build_parser.add_argument("--engine", type=Path, required=True)
    build_parser.add_argument("--output", type=Path, default=ROOT / "dist")
    build_parser.set_defaults(run=build)
    assemble_parser = commands.add_parser("assemble", help="verify and combine target packages")
    assemble_parser.add_argument("--input", type=Path, default=ROOT / "dist")
    assemble_parser.add_argument("--output", type=Path, default=ROOT / "dist")
    assemble_parser.add_argument("--allow-partial", action="store_true")
    assemble_parser.add_argument("--npm-local", action="store_true", help="also emit a local-only npm installer and formula for smoke tests")
    assemble_parser.set_defaults(run=assemble)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
