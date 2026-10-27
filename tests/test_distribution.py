"""Distribution format/integrity tests. Real installer tests live in smoke_packages.py."""

import argparse
import base64
import csv
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest
import zipfile

SPEC = importlib.util.spec_from_file_location("distribute", Path(__file__).resolve().parents[1] / "scripts/distribute.py")
D = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(D)


def elf(machine=62, program_kind=1, dependency=False):
    data = bytearray(144)
    data[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", data, 18, machine)
    struct.pack_into("<Q", data, 32, 64)
    struct.pack_into("<HH", data, 54, 56, 1)
    struct.pack_into("<I", data, 64, program_kind)
    struct.pack_into("<Q", data, 72, 128)
    struct.pack_into("<Q", data, 96, 16)
    if dependency:
        struct.pack_into("<Q", data, 128, 1)  # DT_NEEDED
    return bytes(data)


class DistributionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.output = Path(self.temporary.name)
        self.files = D.common_files() | {"docket": (elf(), 0o755), "oasdiff": (elf(), 0o755)}
        self.target = "x86_64-unknown-linux-musl"

    def test_reject_wrong_cpu_dynamic_loader_and_shared_dependencies(self):
        D.validate_binary(elf(), self.target)
        # Static PIE can have a relocation section, but cannot need shared libs.
        D.validate_binary(elf(program_kind=2), self.target)
        for data in (elf(machine=183), elf(program_kind=3), elf(program_kind=2, dependency=True)):
            with self.assertRaises(ValueError):
                D.validate_binary(data, self.target)
        with self.assertRaises(ValueError):
            D.validate_binary(elf(), "aarch64-apple-darwin")

    def test_wheel_records_tags_licenses_and_executable_permissions(self):
        path = D.build_wheel(self.output, self.target, self.files)
        original = path.read_bytes()
        D.build_wheel(self.output, self.target, self.files)
        self.assertEqual(path.read_bytes(), original)
        with zipfile.ZipFile(path) as wheel:
            info = f"docket-{D.VERSION}.dist-info"
            records = list(csv.reader(io.StringIO(wheel.read(f"{info}/RECORD").decode())))
            self.assertEqual({row[0] for row in records}, set(wheel.namelist()))
            for name, digest, size in records:
                if name.endswith("/RECORD"):
                    self.assertEqual((digest, size), ("", ""))
                    continue
                data = wheel.read(name)
                encoded = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
                self.assertEqual(digest, "sha256=" + encoded)
                self.assertEqual(int(size), len(data))
            for name in ("docket", "oasdiff"):
                self.assertEqual(wheel.getinfo(f"docket_cli/bin/{name}").external_attr >> 16 & 0o777, 0o755)
            self.assertIn("licenses/OASDIFF-LICENSE", "\n".join(wheel.namelist()))
            metadata = wheel.read(f"{info}/METADATA").decode()
            self.assertNotIn("Requires-Dist:", metadata)
            self.assertIn("Requires-Python: >=3.9", metadata)
            tags = wheel.read(f"{info}/WHEEL").decode()
            self.assertIn("Tag: py3-none-manylinux_2_17_x86_64\n", tags)
            self.assertIn("Tag: py3-none-musllinux_1_2_x86_64\n", tags)

    def test_debian_container_control_architecture_and_payload(self):
        path = D.build_deb(self.output, self.target, self.files)
        raw = path.read_bytes()
        self.assertTrue(raw.startswith(b"!<arch>\n"))
        offset, members = 8, {}
        while offset < len(raw):
            header = raw[offset:offset + 60]
            size = int(header[48:58])
            name = header[:16].decode().strip().rstrip("/")
            self.assertEqual(header[-2:], b"`\n")
            members[name] = raw[offset + 60:offset + 60 + size]
            offset += 60 + size + size % 2
        self.assertEqual(list(members), ["debian-binary", "control.tar.gz", "data.tar.gz"])
        self.assertEqual(members["debian-binary"], b"2.0\n")
        with tarfile.open(fileobj=io.BytesIO(members["control.tar.gz"])) as control:
            text = control.extractfile("control").read().decode()
            self.assertIn("Architecture: amd64\n", text)
            self.assertNotIn("Depends: rust", text)
        with tarfile.open(fileobj=io.BytesIO(members["data.tar.gz"])) as payload:
            self.assertEqual(payload.extractfile("usr/lib/docket/docket").read(), elf())
            self.assertIn(b"exec /usr/lib/docket/docket", payload.extractfile("usr/bin/docket").read())
            self.assertEqual(payload.getmember("usr/lib/docket/oasdiff").mode, 0o755)
            self.assertIn("usr/share/doc/docket/OASDIFF-LICENSE", payload.getnames())
            for member in payload:
                for parent in Path(member.name).parents:
                    if str(parent) != ".":
                        self.assertTrue(payload.getmember(str(parent)).isdir())

    def test_npm_platform_selection_without_install_scripts(self):
        package = D.build_npm_platform(self.output, self.target, self.files)
        with tarfile.open(package) as archive:
            metadata = json.load(archive.extractfile("package/package.json"))
            self.assertEqual(metadata["os"], ["linux"])
            self.assertEqual(metadata["cpu"], ["x64"])
            self.assertNotIn("scripts", metadata)
            self.assertEqual(archive.getmember("package/bin/docket").mode, 0o755)
            self.assertIn("package/bin/oasdiff", archive.getnames())

    def create_manifest(self):
        artifacts = {}
        for kind, name in (("archive", "native.tar.gz"), ("npm", "native.tgz"), ("wheel", "native.whl"), ("deb", "native.deb")):
            data = b"fixture artifact " + kind.encode()
            (self.output / name).write_bytes(data)
            artifacts[kind] = {"file": name, "sha256": D.digest(data)}
        manifest = {"target": self.target, "version": D.VERSION,
                    "engine_version": D.ENGINE_VERSION, "artifacts": artifacts}
        (self.output / f"manifest-{self.target}.json").write_bytes(D.json_bytes(manifest))
        return manifest

    def args(self, **extra):
        return argparse.Namespace(input=self.output, output=self.output,
                                  allow_partial=True, npm_local=True, **extra)

    def test_assembly_verifies_checksums_and_generates_pinned_installers(self):
        manifest = self.create_manifest()
        D.assemble(self.args())
        formula = (self.output / "docket.rb").read_text()
        self.assertIn(manifest["artifacts"]["archive"]["sha256"], formula)
        self.assertIn('bin.write_exec_script libexec/"docket"', formula)
        self.assertNotIn('depends_on "rust"', formula)
        with zipfile.ZipFile(self.output / f"docket_release-{D.VERSION}-py3-none-any.whl") as installer:
            metadata = installer.read(f"docket_release-{D.VERSION}.dist-info/METADATA").decode()
            self.assertIn("#sha256=" + manifest["artifacts"]["wheel"]["sha256"], metadata)
            self.assertIn('sys_platform == "linux" and platform_machine == "x86_64"', metadata)
        for kind, dependency in (("", D.BASE + "/native.tgz"), ("-registry", D.VERSION), ("-local", (self.output / "native.tgz").resolve().as_uri())):
            with tarfile.open(self.output / f"docket-npm{kind}-{D.VERSION}.tgz") as archive:
                metadata = json.load(archive.extractfile("package/package.json"))
                self.assertEqual(list(metadata["optionalDependencies"].values()), [dependency])
                self.assertNotIn("scripts", metadata)
        (self.output / "native.whl").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            D.assemble(self.args())

    def test_assembly_rejects_incomplete_releases_and_path_traversal(self):
        manifest = self.create_manifest()
        args = self.args()
        args.allow_partial = False
        with self.assertRaisesRegex(ValueError, "all four"):
            D.assemble(args)
        manifest["artifacts"]["wheel"]["file"] = "../outside.whl"
        (self.output / f"manifest-{self.target}.json").write_bytes(D.json_bytes(manifest))
        with self.assertRaisesRegex(ValueError, "unsafe"):
            D.assemble(self.args())

    def test_tar_output_is_reproducible_and_has_no_host_identity(self):
        first, second = D.tar_bytes(self.files), D.tar_bytes(dict(reversed(list(self.files.items()))))
        self.assertEqual(first, second)
        with tarfile.open(fileobj=io.BytesIO(first)) as archive:
            for member in archive:
                self.assertEqual((member.uid, member.gid, member.uname, member.gname), (0, 0, "", ""))


if __name__ == "__main__":
    unittest.main()
