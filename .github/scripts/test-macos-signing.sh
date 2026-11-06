#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd -P)
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
app="$work_dir/bundle/macos/RillSQL.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$work_dir/bundle/dmg"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>app.rillsql.signing-test</string>
<key>CFBundleExecutable</key><string>rillsql</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>0.0.0</string>
</dict></plist>
PLIST
printf 'int main(void) { return 0; }\n' | xcrun clang -x c - -o "$app/Contents/MacOS/rillsql"
printf 'signed resource\n' > "$app/Contents/Resources/probe.txt"
hdiutil create -quiet -fs HFS+ -volname RillSQL \
  -srcfolder "$work_dir/bundle/macos" -format UDZO \
  "$work_dir/bundle/dmg/RillSQL_0.0.0_aarch64.dmg"

security list-keychains -d user > "$work_dir/keychains-before"
security default-keychain -d user > "$work_dir/default-before"
bash "$script_dir/sign-macos-bundles.sh" "$work_dir/bundle" 0.0.0
security list-keychains -d user > "$work_dir/keychains-after"
security default-keychain -d user > "$work_dir/default-after"
diff -u "$work_dir/keychains-before" "$work_dir/keychains-after"
diff -u "$work_dir/default-before" "$work_dir/default-after"
bash "$script_dir/verify-macos-bundles.sh" "$work_dir/bundle" 0.0.0

printf 'modified resource\n' > "$app/Contents/Resources/probe.txt"
if bash "$script_dir/verify-macos-bundles.sh" "$work_dir/bundle" 0.0.0 > "$work_dir/tampered.log" 2>&1; then
  echo "Signature verification accepted a modified app." >&2
  exit 1
fi
grep -Fq 'sealed resource is missing or invalid' "$work_dir/tampered.log"
echo "macOS signing smoke test passed (app, DMG, updater, tamper rejection and keychain restoration)."
