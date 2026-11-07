# GPUI 底座证据（T1）

日期：2026-11-07。宿主：Windows x86_64。这是实现阶段在本机跑过的检查，不是正式 delivery verification，也不代表 macOS 或 Linux 已通过。

## 依赖与入口

- `gpui-kit` 0.9.0，特性 `tree-sitter-sql`。普通依赖树里的特性是 `assets,component,default,tree-sitter-sql`。没有启用 `gpui-fast`，也没有启用 `test-support`。
- `gpui-pre`、`gpui-pre-platform`、`gpui-pre-macros` 都是 0.3.11。`gpui-pre-platform` 启用 `font-kit`、`x11`、`wayland`、`runtime_shaders`。解析到的 `gpui-pre` 特性含 `windows-manifest`。
- `cargo tree --manifest-path src-tauri/Cargo.toml --locked -e normal -i gpui-fast` 输出 `nothing to print`。锁文件里的 `gpui-fast` 只是 `gpui-kit` 的可选依赖条目。
- 二进制入口是 `src-tauri/src/main.rs` → `app::run()` → `gpui_platform::application().run`。这条路径不启动 Node、Vite 或 WebView。`#[tauri::command]`、`generate_context!` 和领域模块仍留在库 crate，本任务没有删除它们。
- Windows 链接时去掉了 Tauri 的第二份应用 manifest（`WindowsAttributes::new_without_app_manifest`），保留 GPUI 的 manifest，以及 Tauri 的图标、版本和 `generate_context!`。

## 本机已执行

### 窗口启动

```
cargo run --manifest-path src-tauri/Cargo.toml --bin rillsql --offline --locked -- --gpui-smoke
```

退出码 0。标准输出：

```
gpui-window: starting kit=0.9.0 pre=0.3.11 os=windows arch=x86_64
gpui-window: stopped os=windows
```

`--gpui-smoke` 打开 GPUI 窗口后立刻退出。它证明 Windows 上窗口进程能起来并停下来，不是人工走查菜单、主题或快捷键。dev 构建只出现库 crate 既有的 13 条 warning，二进制 `app/` 与 `ui/` 没有新增 warning。

### Headless / 单元测试

```
cargo test --manifest-path src-tauri/Cargo.toml --bin rillsql --locked -- --test-threads=8
```

退出码 0。返修后为 13 passed，0 failed，耗时 0.96s。测试编译同样只出现上述库 warning。打开/保存的 `Ok(None)` 仍是取消；通道关闭和平台 `Err` 写入状态栏。只读网格在单元格点击或方向键之后复制当前格，不回落到默认 `(0, 0)`。矩形选择复制仍然保留。

覆盖：

- 命令标识与现有外壳一致，中英文字典键成对且不回显键名。主题 light/dark/system、密度 compact/standard/relaxed、焦点和命令分发。
- Headless 窗口里安装菜单与动作，切换深色主题、紧凑密度、命令面板焦点和英文文案。
- Editor：`EntityInputHandler` 的 `replace_and_mark_text_in_range` / `unmark_text` 写入「中文」，`secondary-z` 撤销；8000 行 SQL（前后各 4000 行，中间 `/*MARKER-长SQL*/`）上的选区；补全菜单出现 `SELECT`，过期补全被拒绝；lint「未闭合的字符串」写入且过期诊断被拒绝；折叠图标点击后隐藏下一行，再次点击展开。
- DataTable（不是普通 Table）：10000 行只读夹具滚动后可见窗口小于 500 行，矩形选择、TSV 复制与剪贴板、列宽 fit 与手动列宽；1000 行可编辑夹具取消编辑不写回，提交后保留「已改」，再滚到末行。
- 文件：临时目录里的真实文件 `查询.sql` 经 canonicalize 得到绝对路径；不存在的路径被拒绝。打开/保存使用 GPUI `TestAppContext` 的 `simulate_path_prompt_response` 与 `simulate_new_path_selection`。拖放使用测试窗口派发的 `FileDropEvent::Entered` + `Submit`，记录的路径是 canonicalize 后的 `拖放.sql`。

## 未完成限制

- macOS 未执行。`font-kit` 与 `runtime_shaders` 已启用，没有 macOS 窗口、Metal 或 font-kit 运行结果。
- Linux 未执行。`x11` 与 `wayland` 已同时启用，没有 Linux 窗口或两种显示服务器的运行结果。
- 操作系统中文 IME 候选窗未执行。编辑器证据来自 GPUI 输入处理器协议和 headless 窗口，不是系统候选条。
- Windows 通用文件对话框未执行。打开/保存证据是测试平台模拟回调加上真实临时文件路径，不是资源管理器对话框。
- 资源管理器拖放到真实窗口未执行。拖放证据是 headless 测试里派发的 `FileDropEvent`，路径本身来自临时文件。
- 没有同版本性能基线，因此不记录滚动或编辑耗时对比。
- 未运行 `cargo test --lib`。库在二进制测试和 `--gpui-smoke` 编译时通过，带有 13 条既有 warning。未把全包 `cargo clippy -D warnings` 当作本任务通过结果；该命令会检查 T1 范围外的领域模块。
