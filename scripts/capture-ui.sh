#!/usr/bin/env bash
# macOS 原生窗口截图（R16 / R20 取证）。
#
# 用隔离的资料目录（RILLSQL_DEBUG_DATA_DIR）启动一个 debug 构建的 RillSQL，按参数写好主题、语言、会话，
# 可选登记一个 SQLite 演示库；窗口出现后按指定尺寸摆好，用 `screencapture -l <窗口号>` 只截 RillSQL 窗口。
#
# 用法：
#   scripts/capture-ui.sh <nn> <state>
#
# 输出：$OUT_DIR/<nn>-<state>-<width>-<theme>-<locale>.png
#
# 环境变量（都可省略）：
#   RILLSQL_BIN    rillsql 可执行文件（只认 debug 构建：RILLSQL_DEBUG_DATA_DIR 只在 debug 构建生效）。
#                  默认 $CARGO_TARGET_DIR/debug/rillsql，其次 src-tauri/target/debug/rillsql。
#   OUT_DIR        默认 docs/verification/ux-pain-points-2026-11-20
#   WIDTH HEIGHT   窗口尺寸（点），默认 1280 x 820。窄窗口用 WIDTH=760 或 640。
#   THEME          light | dark，默认 light
#   LOCALE         zh-CN | en，默认 zh-CN
#   DENSITY        compact | comfortable，默认 compact
#   SESSION_JSON   一个 JSON 文件，内容是 app-state 里的 "session" 对象（tabs / activeId / pinned）。
#   SQLITE_DB      一个 SQLite 文件：登记为连接 demo（名称「演示库」），会话里可以用 demo|演示库|main||<表>|sqlite|dev 打开。
#   EXTRA_CONN_SQL 追加执行到 connections.sqlite 的 SQL（例如登记开发库 MySQL / Redis 连接）。
#   WAIT           调整窗口后额外等待的秒数，可显式设为 0。默认等窗口位置与尺寸稳定；
#                  指定会话、演示库或额外连接时默认等 4 秒，留出异步加载时间。
#   INTERACTIVE=1  截图前停下，等你在窗口里操作到目标场景后按回车（右键菜单、悬停等手动场景）。
#   KEEP=1         截完不退出应用。
#   RILLSQL_DEBUG_DATA_DIR  资料目录；默认每次新建一个临时目录（不碰你的正式数据）。
#
# 前提：
#   - 系统设置 → 隐私与安全性 → 屏幕录制，允许运行本脚本的终端；否则截到的是空白或桌面。
#   - 调整窗口尺寸用 System Events，需要「辅助功能」权限；没有权限时保留默认尺寸并给出提示（文件名仍按实际宽度）。
#   - 屏幕不能锁定：锁屏时窗口不出帧。
set -euo pipefail

NN="${1:?用法：capture-ui.sh <nn> <state>}"
STATE="${2:?用法：capture-ui.sh <nn> <state>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "capture-ui.sh 只支持 macOS（依赖 screencapture）" >&2
    exit 2
fi

default_bin() {
    if [[ -n "${CARGO_TARGET_DIR:-}" && -x "$CARGO_TARGET_DIR/debug/rillsql" ]]; then
        echo "$CARGO_TARGET_DIR/debug/rillsql"
    elif [[ -x "$HOME/.cache/cargo-target/debug/rillsql" ]]; then
        echo "$HOME/.cache/cargo-target/debug/rillsql"
    else
        echo "$ROOT/src-tauri/target/debug/rillsql"
    fi
}

BIN="${RILLSQL_BIN:-$(default_bin)}"
OUT_DIR="${OUT_DIR:-$ROOT/docs/verification/ux-pain-points-2026-11-20}"
WIDTH="${WIDTH:-1280}"
HEIGHT="${HEIGHT:-820}"
THEME="${THEME:-light}"
LOCALE="${LOCALE:-zh-CN}"
DENSITY="${DENSITY:-compact}"
if [[ -n "${SESSION_JSON:-}${SQLITE_DB:-}${EXTRA_CONN_SQL:-}" ]]; then
    WAIT="${WAIT:-4}"
fi

[[ -x "$BIN" ]] || { echo "找不到 rillsql 可执行文件：$BIN（先 cargo build --manifest-path src-tauri/Cargo.toml --bin rillsql）" >&2; exit 2; }
mkdir -p "$OUT_DIR"

DATA_DIR="${RILLSQL_DEBUG_DATA_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/rillsql-capture.XXXXXX")}"
mkdir -p "$DATA_DIR"

# ---------------------------------------------------------------- 资料目录：app-state.json 与连接
python3 - "$DATA_DIR" "$THEME" "$LOCALE" "$DENSITY" "${SESSION_JSON:-}" <<'PY'
import json, os, sys
data, theme, locale, density, session_path = sys.argv[1:6]
session = {"tabs": [], "activeId": None}
if session_path:
    with open(session_path) as handle:
        session = json.load(handle)
state = {
    "version": 2,
    "shell": {"theme": theme, "density": density, "editorFont": 13, "editorTheme": "auto", "locale": locale,
              "pageSize": 100, "previewKb": 1, "objectPage": 500, "nullCopy": "NULL",
              "queryTimeoutSec": 0, "autoCheckUpdates": False},
    "session": session,
}
with open(os.path.join(data, "app-state.json"), "w") as handle:
    json.dump(state, handle, ensure_ascii=False)
PY

if [[ -n "${SQLITE_DB:-}" || -n "${EXTRA_CONN_SQL:-}" ]]; then
    # 与 connections/store.rs 的建表语句一致；应用启动时 CREATE IF NOT EXISTS 不会覆盖。
    sqlite3 "$DATA_DIR/connections.sqlite" <<'SQL'
CREATE TABLE IF NOT EXISTS connections (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, engine TEXT NOT NULL, env TEXT NOT NULL,
    host TEXT, port INTEGER, database_name TEXT, username TEXT, file_path TEXT,
    tls_mode TEXT, tls_server_name TEXT, ca_mode TEXT, ca_file TEXT, client_cert TEXT, client_key TEXT,
    ssh_host TEXT, ssh_port INTEGER, ssh_user TEXT, ssh_auth TEXT, ssh_key_path TEXT,
    password_saved INTEGER NOT NULL, ssh_password_saved INTEGER NOT NULL, replica_set TEXT
);
SQL
    if [[ -n "${SQLITE_DB:-}" ]]; then
        sqlite3 "$DATA_DIR/connections.sqlite" \
            "INSERT OR REPLACE INTO connections (id, name, engine, env, file_path, password_saved, ssh_password_saved) VALUES ('demo', '演示库', 'sqlite', 'dev', '$(cd "$(dirname "$SQLITE_DB")" && pwd)/$(basename "$SQLITE_DB")', 0, 0);"
    fi
    if [[ -n "${EXTRA_CONN_SQL:-}" ]]; then
        sqlite3 "$DATA_DIR/connections.sqlite" "$EXTRA_CONN_SQL"
    fi
fi

# ---------------------------------------------------------------- 窗口号：CGWindowListCopyWindowInfo
HELPER_DIR="${TMPDIR:-/tmp}/rillsql-capture-helper"
HELPER="$HELPER_DIR/window-id-v2"
if [[ ! -x "$HELPER" ]]; then
    mkdir -p "$HELPER_DIR"
    cat >"$HELPER_DIR/window-id.swift" <<'SWIFT'
import CoreGraphics
import Foundation
// 用法：window-id <pid>。输出这个进程最大的一个普通窗口：<窗口号> <宽> <高> <x> <y>（点）。
let pid = Int32(CommandLine.arguments[1]) ?? -1
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
var best: (Int, Double, Double, Double, Double)? = nil
for info in list {
    guard (info[kCGWindowOwnerPID as String] as? Int32) == pid,
          (info[kCGWindowLayer as String] as? Int) == 0,
          let number = info[kCGWindowNumber as String] as? Int,
          let bounds = info[kCGWindowBounds as String] as? [String: Double] else { continue }
    let width = bounds["Width"] ?? 0, height = bounds["Height"] ?? 0
    if best == nil || width * height > best!.1 * best!.2 { best = (number, width, height, bounds["X"] ?? 0, bounds["Y"] ?? 0) }
}
if let best = best { print("\(best.0) \(Int(best.1)) \(Int(best.2)) \(Int(best.3)) \(Int(best.4))") } else { exit(1) }
SWIFT
    swiftc -O -o "$HELPER" "$HELPER_DIR/window-id.swift" >/dev/null
fi

# ---------------------------------------------------------------- 启动、摆窗口、截图
# 锁屏时窗口不出帧，截到的是锁屏画面：直接停下。
if ioreg -n Root -d1 -a 2>/dev/null | grep -A1 CGSSessionScreenIsLocked | grep -q '<true/>'; then
    echo "屏幕已锁定，窗口不出帧：解锁后再截图" >&2
    exit 3
fi
RILLSQL_DEBUG_DATA_DIR="$DATA_DIR" "$BIN" >"$DATA_DIR/rillsql.log" 2>&1 &
PID=$!
cleanup() {
    if [[ "${KEEP:-0}" != "1" ]]; then
        kill "$PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

WINDOW=""
for attempt in $(seq 1 64); do
    kill -0 "$PID" 2>/dev/null || break
    if WINDOW="$("$HELPER" "$PID" 2>/dev/null)"; then
        break
    fi
    if (( attempt <= 5 )); then sleep 0.1; else sleep 0.5; fi
done
[[ -n "$WINDOW" ]] || { echo "没有找到 RillSQL 窗口（pid $PID）。日志：$DATA_DIR/rillsql.log" >&2; exit 1; }

# 按指定尺寸摆窗口（需要辅助功能权限）；失败时保留默认尺寸。
osascript >/dev/null 2>&1 <<OSA || echo "提示：无法调整窗口尺寸（辅助功能权限？），按默认尺寸截图" >&2
tell application "System Events"
    set target to first process whose unix id is $PID
    set frontmost of target to true
    tell window 1 of target
        set position to {40, 60}
        set size to {$WIDTH, $HEIGHT}
    end tell
end tell
OSA

WINDOW=""
if [[ "${INTERACTIVE:-0}" == "1" ]]; then
    read -r -p "在 RillSQL 窗口里操作到「$STATE」场景后按回车截图……" _
elif [[ -n "${WAIT:-}" ]]; then
    sleep "$WAIT"
else
    # System Events 返回时窗口可能还在移动或缩放。用两次相同的窗口号和边界确认稳定。
    previous=""
    for _ in $(seq 1 20); do
        kill -0 "$PID" 2>/dev/null || break
        current="$("$HELPER" "$PID" 2>/dev/null)" || current=""
        if [[ -n "$current" && "$current" == "$previous" ]]; then
            WINDOW="$current"
            break
        fi
        previous="$current"
        sleep 0.2
    done
    if [[ -z "$WINDOW" ]]; then
        echo "窗口仍在变化，按截图时的实际尺寸取景。" >&2
    fi
fi

if [[ -z "$WINDOW" ]]; then
    WINDOW="$("$HELPER" "$PID")" || { echo "截图前未找到 RillSQL 窗口。日志：$DATA_DIR/rillsql.log" >&2; exit 1; }
fi
read -r WINDOW_ID ACTUAL_W ACTUAL_H WIN_X WIN_Y <<<"$WINDOW"
FILE="$OUT_DIR/$NN-$STATE-$ACTUAL_W-$THEME-$LOCALE.png"
# 按窗口号截；个别系统版本对 GPU 窗口报「could not create image from window」时，退回按窗口矩形截（窗口须在最前）。
if ! screencapture -x -o -l "$WINDOW_ID" "$FILE" 2>/dev/null; then
    screencapture -x -R "$WIN_X,$WIN_Y,$ACTUAL_W,$ACTUAL_H" "$FILE"
fi
echo "$FILE (${ACTUAL_W}x${ACTUAL_H}, data=$DATA_DIR, pid=$PID)"
