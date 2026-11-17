# 新建 / 编辑连接表单打磨

基线：`8810db4`（v0.2.40）。范围：新建、编辑、复制连接共用的那个模态表单。

**取证方式与局限**：这一轮在 macOS 上做，终端没有屏幕录制和辅助功能权限，无法截图，也无法在原生窗口里点按。
问题来自读代码，结论来自 Rust 测试——其中 `the_modal_keeps_verdicts_and_the_discard_prompt_above_the_footer`
是 GPUI 真实绘制（真实点击、按键、元素几何）。**没有人眼看过改动后的界面**：间距、配色、换行等观感需要在原生应用里再过一遍。

## 修复与改进

| # | 问题 | 处理 |
|---|---|---|
| 1 | **表单上没有“保存密码”的选择**。草稿里有 `save_password`，但没有任何控件能改它：复制出来或导入的连接（不带密码）编辑时输入密码再保存，密码被静静丢掉，这条连接永远要每次询问 | 身份验证区加“保存密码”开关，旁边写明当前状态的后果（存入钥匙串 / 每次启动后首次连接时询问 / 保存后会删除已存的密码）。开关进入 Tab 序列 |
| 2 | **一次 Esc 丢掉整张表单**。填了十几个字段后误按 Esc 或点“取消”，草稿和密码直接没了 | 有未保存的修改时先在页脚上方问一句（继续编辑 / 放弃修改）；键盘落在“继续编辑”上，再按 Esc 也是继续编辑。空表单、只选了类型或环境、没动过的已有连接和副本照旧直接关 |
| 3 | **换数据库类型等于清空表单**（只留名称）。填好主机、用户、跳板机后想从 MySQL 改成 MariaDB 要全部重填 | 联网引擎之间沿用主机、用户名、默认库、TLS 与 SSH；端口、环境、TLS 模式只在用户改过时沿用，否则换成新引擎的默认值。SQLite 与联网引擎之间只留名称和环境；Redis 的库索引、整串 MongoDB URI 不跨引擎沿用 |
| 4 | **测试 / 保存的结果画在滚动区末尾**，表单一长（开了 TLS 校验或 SSH）就在视口外，点了“测试连接”只看到转圈消失 | 结论固定在页脚上方，不随主体滚动。测试结论写出卡在哪一步、服务端原话和处理建议，并列出走过的各阶段（网络 / TLS / 登录…） |
| 5 | **改了字段后仍挂着上一份配置的“连接成功”** | 草稿或密码一改，结论收起；“凭据被拒”的提示在改了用户名或密码后收起 |
| 6 | **连点保存会存出两条连接**：保存任务没有去重，新建草稿在第一次结果回来前没有 id | 保存在路上时不接受第二次；对应按钮转圈并显示“正在保存…”，另外两个动作变淡。失败后恢复可点 |
| 7 | 上一次粘贴的连接串（可能带密码）留在下一张表单的连接串框里 | 打开和关闭表单时清空 |
| 8 | 页脚四个按钮并排、同等权重，测试夹在取消和保存中间 | 测试在左（带图标，测试中写“正在测试连接…”），取消 / 保存 / 保存并连接在右，主操作最右 |
| 9 | 只能用鼠标点“保存并连接” | Cmd/Ctrl+Enter 在任何字段里都是保存并连接（按钮提示里写明） |
| 10 | 保存或测试被校验拦下时，出错的字段可能已滚出视口 | 键盘落到第一个要改的字段 |
| 11 | 连接串解析成功的提示只在状态栏（模态背后） | 连接串下方就地提示，连接串再被改动就收起 |
| 12 | 副本固定叫 “名称 copy”，中文界面里是英文后缀，且会重名 | 按界面语言命名（“名称 副本”），已存在则编号 |
| 13 | 编辑 / 复制时键盘落在用不上的连接串框；端口清空后看不到默认端口 | 键盘落在名称上；端口占位显示该引擎的默认端口 |

## 验证

- `cargo test --manifest-path src-tauri/Cargo.toml --locked`：1225 passed，0 failed，9 ignored（本机没有 Docker，PostgreSQL / MySQL 等夹具不在线，远程库上的行为这一轮没有覆盖）。
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked`：0 告警。
- 新增测试：`switching_engine_keeps_what_still_applies`、`an_untouched_new_draft_is_recognised`、
  `touching_the_draft_retires_the_last_test_verdict`、`the_discard_prompt_adds_its_two_buttons_before_the_footer`、
  `closing_a_filled_form_asks_before_discarding`、`a_second_save_is_ignored_until_the_first_one_answers`、
  `the_save_password_switch_decides_whether_the_password_is_kept`（真实配置库 + 内存钥匙串）、
  `duplicates_are_named_uniquely_and_the_url_box_starts_empty`、`a_test_verdict_is_retired_when_the_draft_changes`（真实 SQLite 测试连接）、
  `the_modal_keeps_verdicts_and_the_discard_prompt_above_the_footer`（GPUI 绘制）。
- 改动的既有测试：`sidebar_copy_and_test_remain_usable_while_catalog_is_loading` 原先断言副本名以 ` copy` 结尾，改为按界面语言的副本名。

## 未做

- 原生应用里的目视检查与截图（见上）。
- 测试结论里的阶段名来自服务层，只有中文；英文界面下这一行仍是中文（与此前的失败原因一致）。
- 远程数据库上的测试连接没有跑（无 Docker）；失败结论的版式用构造的报告在绘制测试里验证。
