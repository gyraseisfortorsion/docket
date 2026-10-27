#!/bin/bash
# Test a generated local formula in an isolated temporary tap. No publishing.
set -euo pipefail
formula="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
if brew list --versions docket >/dev/null 2>&1; then
  echo "Remove the existing Homebrew docket installation before running this smoke test." >&2
  exit 1
fi
tap_dir="$(mktemp -d)"
tap_name="docket-ci/packaging-test-$$"
cleanup() {
  brew uninstall --force "$tap_name/docket" >/dev/null 2>&1 || true
  brew untap "$tap_name" >/dev/null 2>&1 || true
  rm -rf "$tap_dir"
}
trap cleanup EXIT
mkdir -p "$tap_dir/Formula"
cp "$formula" "$tap_dir/Formula/docket.rb"
git -C "$tap_dir" init -q
git -C "$tap_dir" add Formula/docket.rb
git -C "$tap_dir" -c user.name=Docket -c user.email=noreply@github.com commit -qm "Local packaging test"
export HOMEBREW_DEVELOPER=1
export HOMEBREW_NO_AUTO_UPDATE=1
export HOMEBREW_NO_INSTALL_CLEANUP=1
brew tap "$tap_name" "$tap_dir"
brew install "$tap_name/docket"
brew test "$tap_name/docket"
