"""Executed by the project's interpreter; imports FastAPI only through the app."""
import contextlib
import importlib
import importlib.machinery
import io
import json
import os
import pathlib
import sys
import time

settings = json.loads(sys.argv[1])
root = pathlib.Path(settings["root"]).resolve()
original = pathlib.Path(settings.get("original_repository", settings["root"])).resolve()
paths = [str((root / p).resolve()) for p in settings["import_paths"]]

def repository_path(location):
    path = pathlib.Path(location).resolve()
    try:
        path.relative_to(original)
    except ValueError:
        return False
    return "site-packages" not in path.parts and "dist-packages" not in path.parts

# Namespace packages combine all matching sys.path directories. Prepending alone
# would still let an editable install add unstaged namespace search locations.
if root != original:
    sys.path[:] = [p for p in sys.path if not repository_path(p)]
sys.path[:0] = paths

# Timestamp-based .pyc files can be stale after same-size edits within a second.
# Always compile repository source; leave dependency bytecode caching intact.
sys.dont_write_bytecode = True
cached_get_code = importlib.machinery.SourceFileLoader.get_code

def fresh_repository_code(loader, fullname):
    parts = pathlib.Path(loader.path).parts
    if "site-packages" in parts or "dist-packages" in parts:
        return cached_get_code(loader, fullname)
    path = pathlib.Path(loader.path).resolve()
    try:
        path.relative_to(root)
        staged = True
    except ValueError:
        staged = False
    if staged or repository_path(path):
        return loader.source_to_code(loader.get_data(loader.path), loader.path)
    return cached_get_code(loader, fullname)

importlib.machinery.SourceFileLoader.get_code = fresh_repository_code

def check_imports():
    if root == original:
        return
    for name, module in list(sys.modules.items()):
        locations = list(getattr(module, "__path__", []))
        filename = getattr(module, "__file__", None)
        if filename:
            locations.append(filename)
        for location in locations:
            path = pathlib.Path(location).resolve()
            try:
                relative = path.relative_to(original)
            except ValueError:
                continue
            # Dependencies in the original project's environment are intentional.
            if "site-packages" in path.parts or "dist-packages" in path.parts:
                continue
            raise RuntimeError(
                "repository module %s resolved outside the staged export: %s; "
                "configure import_paths for all repository source roots" % (name, relative)
            )

# Keep application print() calls out of the protocol on stdout.
with contextlib.redirect_stdout(sys.stderr):
    start = time.perf_counter()
    module_name, separator, attribute = settings["app"].partition(":")
    if not separator:
        raise ValueError("FastAPI app must have the form package.module:app")
    try:
        target = importlib.import_module(module_name)
    except ModuleNotFoundError as error:
        if root != original:
            raise RuntimeError(
                "cannot import %s in the staged tree; original source outside the staged "
                "export is excluded; configure import_paths and verify interpreter dependencies"
                % error.name
            ) from error
        raise
    for part in attribute.split("."):
        target = getattr(target, part)
    if settings["factory"]:
        target = target()
    imported = time.perf_counter()
    check_imports()
    schema = target.openapi()
    generated = time.perf_counter()
    check_imports()

json.dump({"document": schema, "timings": {
    "import_ms": (imported - start) * 1000,
    "generation_ms": (generated - imported) * 1000,
}}, sys.stdout, sort_keys=True)

