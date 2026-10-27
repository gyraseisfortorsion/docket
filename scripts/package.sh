#!/bin/sh
# Assemble a local CLI + pinned engine bundle. No publishing or uploads.
set -eu
cargo build --release --locked
bundle_dir="target/contracts-$(uname -s)-$(uname -m)"
mkdir -p "$bundle_dir"
cp target/release/contracts "$bundle_dir/contracts"
engine_path="$(target/release/contracts engine install)"
cp "$engine_path" "$bundle_dir/oasdiff"
cp LICENSE "$bundle_dir/LICENSE"
cp scripts/OASDIFF-LICENSE "$bundle_dir/OASDIFF-LICENSE"
cp README.md "$bundle_dir/README.md"
tar -czf "$bundle_dir.tar.gz" -C "$bundle_dir" .
printf '%s\n' "$bundle_dir.tar.gz"
