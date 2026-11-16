#!/usr/bin/env bash
# 把本地构建装成 macOS 应用（RillSQL.app），之后可以从启动台、聚焦搜索或程序坞直接启动。
#
#   scripts/macos-install-app.sh            # 构建 release 并安装到 ~/Applications
#   scripts/macos-install-app.sh --dock     # 同上，并把 RillSQL 固定到程序坞
#   APP_DIR=/Applications scripts/macos-install-app.sh
#
# 只用系统自带工具组装 .app（不需要发布签名密钥）；正式安装包仍走 rillsql-package。
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "只适用于 macOS。" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/src-tauri/Cargo.toml"
APP_DIR="${APP_DIR:-$HOME/Applications}"
ADD_TO_DOCK=0
for arg in "$@"; do
  case "$arg" in
    --dock) ADD_TO_DOCK=1 ;;
    -h|--help)
      sed -n '2,8p' "$0"
      exit 0
      ;;
    *)
      echo "未知参数：$arg" >&2
      exit 1
      ;;
  esac
done

echo "==> 构建 release 二进制"
cargo build --release --locked --manifest-path "$MANIFEST" --bin rillsql

TARGET_DIR="$(cargo metadata --format-version 1 --no-deps --manifest-path "$MANIFEST" | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
BINARY="$TARGET_DIR/release/rillsql"
if [[ ! -x "$BINARY" ]]; then
  echo "找不到构建产物：$BINARY" >&2
  exit 1
fi
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$MANIFEST" | head -n 1)"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
BUNDLE="$STAGE/RillSQL.app"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"
cp "$BINARY" "$BUNDLE/Contents/MacOS/RillSQL"
chmod 755 "$BUNDLE/Contents/MacOS/RillSQL"
cp "$ROOT/src-tauri/icons/icon.icns" "$BUNDLE/Contents/Resources/icon.icns"
cat > "$BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>zh_CN</string>
  <key>CFBundleDisplayName</key><string>RillSQL</string>
  <key>CFBundleExecutable</key><string>RillSQL</string>
  <key>CFBundleIconFile</key><string>icon.icns</string>
  <key>CFBundleIdentifier</key><string>app.rillsql.desktop</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>RillSQL</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

# Apple Silicon 只运行签过名的代码：给整个包做一次本机（ad-hoc）签名。
codesign --force --deep --sign - "$BUNDLE" >/dev/null 2>&1

mkdir -p "$APP_DIR"
DEST="$APP_DIR/RillSQL.app"
if pgrep -f "$DEST/Contents/MacOS/RillSQL" >/dev/null 2>&1; then
  echo "RillSQL 正在运行，请先退出再安装。" >&2
  exit 1
fi
rm -rf "$DEST"
ditto "$BUNDLE" "$DEST"
xattr -dr com.apple.quarantine "$DEST" 2>/dev/null || true
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
[[ -x "$LSREGISTER" ]] && "$LSREGISTER" -f "$DEST" || true
echo "==> 已安装：$DEST（版本 $VERSION）"

if [[ "$ADD_TO_DOCK" == 1 ]]; then
  if defaults read com.apple.dock persistent-apps 2>/dev/null | grep -q "app.rillsql.desktop\|RillSQL.app"; then
    echo "==> 程序坞里已经有 RillSQL"
  else
    defaults write com.apple.dock persistent-apps -array-add \
      "<dict><key>tile-data</key><dict><key>file-data</key><dict><key>_CFURLString</key><string>file://${DEST// /%20}/</string><key>_CFURLStringType</key><integer>15</integer></dict></dict></dict>"
    killall Dock
    echo "==> 已固定到程序坞"
  fi
fi

echo "从启动台 / 聚焦搜索打开 RillSQL，或运行：open \"$DEST\""
