#!/usr/bin/env python3
"""Generate a real example report with a breaking change and outstanding proposal."""
import argparse
import copy
import json
import pathlib
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/docket")
    parser.add_argument("--output", default="target/demo-report.html")
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve())
    output = pathlib.Path(args.output).absolute()
    example = pathlib.Path(__file__).resolve().parents[1] / "examples/users.openapi.json"
    with tempfile.TemporaryDirectory(prefix="docket-demo-") as directory:
        root = pathlib.Path(directory)

        def run(*argv):
            return subprocess.check_output([binary, *argv], cwd=root, text=True)

        original = json.loads(example.read_text())
        (root / "openapi.json").write_text(json.dumps(original))
        run("init", "--service", "users-api", "--file", "openapi.json")
        run("sync")
        run("propose", "user-contact-details")
        changed = copy.deepcopy(original)
        model = changed["components"]["schemas"]["UserCreate"]
        model["properties"]["email"] = {"type": "string", "format": "email"}
        model["required"].append("email")
        desired = copy.deepcopy(changed)
        desired["components"]["schemas"]["UserCreate"]["properties"]["timezone"] = {"type": "string"}
        proposal = root / ".docket/proposals/user-contact-details/users-api/openapi.json"
        proposal.write_text(json.dumps(desired))
        (root / "openapi.json").write_text(json.dumps(changed))
        run("sync")
        run("report", "--since", "previous", "--format", "html", "--output", str(output))
    print(output)


if __name__ == "__main__":
    main()
