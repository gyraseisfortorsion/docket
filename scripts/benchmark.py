#!/usr/bin/env python3
"""Measure extraction/normalization, storage, and real compatibility separately."""
import argparse
import json
import pathlib
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/release/docket")
    parser.add_argument("--sizes", nargs="+", type=int, default=[100, 1000, 10000])
    parser.add_argument("--fastapi-python", help="Also benchmark fresh FastAPI imports/schema generation with this interpreter")
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve())
    results = []
    for size in args.sizes:
        with tempfile.TemporaryDirectory(prefix="docket-benchmark-") as directory:
            root = pathlib.Path(directory)

            def run(*argv):
                return subprocess.check_output([binary, *argv], cwd=root, text=True)

            document = {"openapi": "3.1.0", "info": {"title": "Benchmark", "version": "1.0.0"}, "paths": {}, "components": {"schemas": {
                "Item": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}
            }}}
            for index in range(size):
                document["paths"]["/items/%d" % index] = {"get": {
                    "tags": ["items"], "operationId": "getItem%d" % index,
                    "responses": {"200": {"description": "An item", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Item"}}}}}
                }}
            source = root / "openapi.json"
            source.write_text(json.dumps(document))
            run("init", "--service", "benchmark", "--file", "openapi.json")
            start = time.perf_counter()
            first = json.loads(run("sync", "--json"))
            first_total = (time.perf_counter() - start) * 1000
            noop = json.loads(run("sync", "--json"))
            document["components"]["schemas"]["Item"]["properties"]["id"]["type"] = "integer"
            source.write_text(json.dumps(document))
            diff = json.loads(run("diff", "--format", "json"))
            changed = json.loads(run("sync", "--json"))
            results.append({
                "source": "file",
                "endpoints": size,
                "source_bytes": source.stat().st_size,
                "first_sync_total_ms": first_total,
                "extraction": first["timings"]["benchmark"],
                "first_storage_ms": first["sync"]["storage_ms"],
                "noop_storage_ms": noop["sync"]["storage_ms"],
                "changed_storage_ms": changed["sync"]["storage_ms"],
                "comparison_ms": diff["comparison"]["comparison_ms"],
                "breaking_findings": sum(f["classification"] == "breaking" for f in diff["comparison"]["findings"]),
            })
        if args.fastapi_python:
            # Preserve the venv executable path: resolving its symlink loses sys.prefix.
            interpreter = str(pathlib.Path(args.fastapi_python).absolute())
            with tempfile.TemporaryDirectory(prefix="docket-fastapi-benchmark-") as directory:
                root = pathlib.Path(directory)

                def run(*argv):
                    return subprocess.check_output([binary, *argv], cwd=root, text=True)

                application = """from fastapi import FastAPI
from pydantic import BaseModel
class Item(BaseModel):
    id: str
app = FastAPI()
def get_item():
    return {'id': 'example'}
for index in range(%d):
    app.add_api_route('/items/%%d' %% index, get_item, response_model=Item, tags=['items'], operation_id='getItem%%d' %% index)
""" % size
                app_path = root / "app.py"
                app_path.write_text(application)
                run("init", "--service", "benchmark", "--fastapi", "app:app", "--python", interpreter)
                first = json.loads(run("sync", "--json"))
                app_path.write_text(application.replace("    id: str", "    id: int"))
                diff = json.loads(run("diff", "--format", "json"))
                changed = json.loads(run("sync", "--json"))
                results.append({
                    "source": "fastapi",
                    "endpoints": size,
                    "extraction": first["timings"]["benchmark"],
                    "first_storage_ms": first["sync"]["storage_ms"],
                    "changed_storage_ms": changed["sync"]["storage_ms"],
                    "comparison_ms": diff["comparison"]["comparison_ms"],
                    "breaking_findings": sum(f["classification"] == "breaking" for f in diff["comparison"]["findings"]),
                })
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
