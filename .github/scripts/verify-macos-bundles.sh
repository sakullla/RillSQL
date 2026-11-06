#!/usr/bin/env bash
set -euo pipefail

bundle_dir="${1:?Usage: verify-macos-bundles.sh BUNDLE_DIR VERSION}"
version="${2:?Version is required}"
: "${APPLE_SIGNING_IDENTITY:?Expected signing identity is required}"
work_dir=$(mktemp -d)
mounted=false
cleanup() {
  if [[ "$mounted" == true ]]; then
    hdiutil detach "$work_dir/dmg" -quiet || true
  fi
  rm -rf "$work_dir"
}
trap cleanup EXIT

verify_app() {
  local signature
  codesign --verify --deep --strict --verbose=2 "$1"
  signature=$(codesign --display --verbose=4 "$1" 2>&1)
  if ! grep -Fqx "Authority=$APPLE_SIGNING_IDENTITY" <<< "$signature"; then
    echo "Unexpected signing identity in $1; expected $APPLE_SIGNING_IDENTITY" >&2
    exit 1
  fi
}

# Check every delivered copy, not just the build's original app. A linker-only
# signature on the Mach-O passes binary checks but leaves the app unsealed.
verify_app "$bundle_dir/macos/RillSQL.app"
mkdir "$work_dir/updater"
tar -xzf "$bundle_dir/macos/RillSQL.app.tar.gz" -C "$work_dir/updater"
verify_app "$work_dir/updater/RillSQL.app"
hdiutil attach "$bundle_dir/dmg/RillSQL_${version}_aarch64.dmg" \
  -readonly -nobrowse -mountpoint "$work_dir/dmg" -quiet
mounted=true
verify_app "$work_dir/dmg/RillSQL.app"

# A stable self-signed identity does not confer Apple Gatekeeper trust.
