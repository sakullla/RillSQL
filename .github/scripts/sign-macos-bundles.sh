#!/usr/bin/env bash
set -euo pipefail
umask 077

bundle_dir="${1:?Usage: sign-macos-bundles.sh BUNDLE_DIR VERSION}"
version="${2:?Version is required}"
: "${APPLE_CERTIFICATE:?A stable signing certificate is required}"
: "${APPLE_CERTIFICATE_PASSWORD:?Certificate password is required}"
: "${APPLE_SIGNING_IDENTITY:?Signing identity is required}"
work_dir=$(mktemp -d)
keychain="$work_dir/signing.keychain-db"
mounted=false
cleanup() {
  if [[ "$mounted" == true ]]; then
    hdiutil detach "$work_dir/dmg" -quiet || true
  fi
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -rf "$work_dir"
}
trap cleanup EXIT

keychain_password=$(openssl rand -hex 32)
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
printf '%s' "$APPLE_CERTIFICATE" | base64 --decode > "$work_dir/certificate.p12"
security import "$work_dir/certificate.p12" -k "$keychain" \
  -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign -T /usr/bin/security
security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
  -k "$keychain_password" "$keychain" >/dev/null
rm "$work_dir/certificate.p12"

app="$bundle_dir/macos/RillSQL.app"
# RillSQL has one native executable and no embedded frameworks or sidecars.
codesign --force --options runtime --timestamp=none --keychain "$keychain" \
  --sign "$APPLE_SIGNING_IDENTITY" "$app/Contents/MacOS/rillsql"
codesign --force --options runtime --timestamp=none --keychain "$keychain" \
  --sign "$APPLE_SIGNING_IDENTITY" "$app"
codesign --verify --deep --strict --verbose=2 "$app"

# Preserve Tauri's Finder layout, replacing only the app inside the disk image.
dmg="$bundle_dir/dmg/RillSQL_${version}_aarch64.dmg"
hdiutil convert "$dmg" -format UDRW -o "$work_dir/writable.dmg" -quiet
size_mib=$(( ($(stat -f %z "$work_dir/writable.dmg") + 1048575) / 1048576 + 64 ))
hdiutil resize -size "${size_mib}m" "$work_dir/writable.dmg" -quiet
hdiutil attach "$work_dir/writable.dmg" -readwrite -nobrowse \
  -mountpoint "$work_dir/dmg" -quiet
mounted=true
rm -rf "$work_dir/dmg/RillSQL.app"
ditto "$app" "$work_dir/dmg/RillSQL.app"
hdiutil detach "$work_dir/dmg" -quiet
mounted=false
hdiutil convert "$work_dir/writable.dmg" -format UDZO -o "$work_dir/signed.dmg" -quiet
mv "$work_dir/signed.dmg" "$dmg"

# The updater signature must be generated only after this archive is final.
tar -czf "$bundle_dir/macos/RillSQL.app.tar.gz" -C "$bundle_dir/macos" RillSQL.app
