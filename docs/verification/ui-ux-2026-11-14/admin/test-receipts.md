# 测试回执（T15）

运行时间：2026-11-14，工作树 HEAD `df79f24`（`feat: 对象比较页统一三态与窄宽重排`）。
本任务的改动未提交，位于该 commit 之上的工作树（同组其它任务也在同一工作树改各自的目录）。

## 1. 本模块用例（全绿）

命令：

```
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::views::admin
```

结果：`running 14 tests` / `test result: ok. 14 passed; 0 failed`（958 filtered out）。

| 用例 | 覆盖 | 结果 |
| --- | --- | --- |
| `probing_is_visible_and_offline_targets_get_a_connect_next_step` | 首屏骨架 → 刷新增量指示 → 未连接空态 + 「立即连接」；三段里操作卡都在；点空态主操作真的发出连接请求 | ok |
| `inventory_faults_use_the_unified_error_state_with_a_retry_next_step` | 两个分区读取失败 → 两个 `error-state`（各自 `admin-fault-{i}` 作用域内），各带已翻译的重试按钮 | ok |
| `empty_sections_carry_a_next_step_and_keep_their_forms` | sqlite 附加库 / postgres 序列为空 → 本节空态 + 说明可见；输入框与预览入口仍在，按钮无障碍名是本节标题 | ok |
| `dangerous_previews_mark_the_confirm_and_explain_that_it_cannot_be_undone` | VACUUM 用提醒措辞、danger 计划（删表预览）换危险措辞；两种提示互斥；确认入口 `admin-confirm` 仍在 | ok |
| `states_and_actions_stay_inside_1360_900_and_640_in_both_themes_and_densities` | 3 宽 × 浅/深 × 紧凑/标准 = 12 组：空态说明、预览入口、刷新按钮都不越出右边界（几何见 `states-and-widths.md`） | ok |
| `admin_state_copy_is_translated_and_state_specific` | 新增 20 个文案键两种语言都存在、英文不含中文；"无操作 / 空 section / 离线"三类措辞互不相同 | ok |
| 既有 8 个用例 | 英文文案无中文、远程目标离线、输入→预览、失败保留预览、目标切换丢弃预览、sqlite inventory 读附加库、PRAGMA 取消/确认、分区错误只落在本分区 | 全部 ok |

## 2. 全量测试

命令：

```
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

结果：`965 passed; 1 failed; 7 ignored`。

- **通过**：`ui::kit::token_tests::style_literals_follow_design_tokens`、`style_scan_rejects_reverted_literals`、
  `ui::kit::state_tests::*`（三态底座）与本页 14 条用例都在通过之列。
- **失败（不属于本任务范围）**：`app::views::query::tests::long_select_keeps_polling_past_ten_seconds`
  （`src/app/views/query/mod.rs:3699`，"超过 10 秒的 SELECT 没有回到结果： polls=0"）。该用例在查询视图，
  本任务没有改动 `views/query/`、计时或轮询逻辑；同一工作树里其它任务正在改各自的视图，这里按实测记录，不认领、不掩盖。
- 前一次全量运行（同一工作树，稍早）还有 `app::views::mongo` 与 `app::views::redis::states` 两条失败，
  随后由对应任务的改动消失；说明这些失败来自并行改动，而不是管理页。

## 3. Lint

命令：

```
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked
```

结果：退出码 0，`0` 条 error；全 crate 20 warning（lib）/ 19 warning（lib test），
**没有一条落在 `src/app/views/admin/mod.rs`**（`grep -c 'views/admin' ` 为 0）。

## 4. 依赖边界

命令：

```
git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock
```

结果：退出码 0，无改动（本任务没有新增依赖）。

## 5. 未执行 / 不计作通过

| 项 | 状态 | 说明 |
| --- | --- | --- |
| 原生像素截图（浅/深 × 紧凑/标准 × 1360/900/640） | **未取得** | 运行环境没有原生窗口与截屏通道。`states-and-widths.md` 给的是 headless 无头窗口里的实际边界，不等于像素证据；按执行计划由 Delivery Owner 在集成 candidate 上补取 |
| 真机 sqlite 附加库空态 / postgres 序列空态的显式截图 | 未取得 | 需要真实 PostgreSQL fixture；本任务用 `inventory_from_extras` 构造同形态清单做 headless 验证，模型侧读取路径由既有用例覆盖 |
| 原生应用里点「重试」后的真实重读 | 未在原生窗口验证 | headless 用例断言了重试按钮存在且文案已翻译，但没有点击触发真实重读（会拉起视图运行时去连库）；重读路径本身沿用未改动的 `refresh` / `read_inventory` |
