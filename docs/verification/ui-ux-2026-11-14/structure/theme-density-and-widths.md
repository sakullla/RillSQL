# 结构页几何：密度、列宽与三档宽度（T9）

证据类型：**源码级 + headless 布局级**，不是原生像素截图（见 `README.md`）。
下表的数值直接来自 `TableSpec::of(Metrics)`、`FIELDS_TABLE / INDEXES_TABLE / FOREIGN_TABLE` 与 `kit::Metrics`，
由 `structure_lists_share_one_row_height_and_fit_the_narrowest_window` 与
`primary_action_stays_outside_the_scrolled_body_at_every_width` 钉住；改任一数值都会让对应用例失败。

## 1. 列表几何（三张表同一份）

| 项目 | 紧凑 | 标准 | 宽松 | 取值 |
| --- | --- | --- | --- | --- |
| 表头高 | 26 | 32 | 40 | `Metrics::row_h`（S12.2：网格表头与行高同取 `row_h`） |
| 行高（最小） | 26 | 32 | 40 | `Metrics::row_h` |
| 左右内边距 | 12 | 12 | 12 | `TableSpec::PAD_X` |
| 表头底色/字色 | `chrome` / `text_faint` | 同左 | 同左 | 双主题取 `Palette` |
| 行底部分隔 | 发丝线 `line_soft` | 同左 | 同左 | |
| hover | `hover` 底 | 同左 | 同左 | |
| 已标记删除的行 | 不透明度 0.5 | 同左 | 同左 | 只表达草稿状态，不执行 |

## 2. 列宽（权重 + 最小宽度）

| 表 | 列 | 权重 | 最小宽 |
| --- | --- | --- | --- |
| 字段 | 字段名（主键带钥匙图标） | 2.0 | 96 |
| 字段 | 类型 | 1.4 | 76 |
| 字段 | 可空 | 0.6 | 52 |
| 字段 | 默认值 | 1.4 | 80 |
| 索引 | 索引名（唯一带 # 图标） | 2.0 | 96 |
| 索引 | 列（降序列标 `DESC`） | 2.0 | 96 |
| 索引 | 唯一 | 0.6 | 52 |
| 外键 | 外键名 | 1.4 | 84 |
| 外键 | 本表列 | 1.4 | 84 |
| 外键 | 引用 `schema.table(col)` | 2.0 | 108 |

最小宽度之和：字段 304、索引 244、外键 276；加上表内边距 24，在 640 宽窗口（紧凑档页面左右各 16）里都放得下。
超出部分按权重分配，所有单元格 `truncate`，最后一列固定右内边距 8。

## 3. 三档宽度行为（headless 布局级）

| 宽度 | 主操作（预览 SQL） | 说明 |
| --- | --- | --- |
| 1360 | 在固定动作栏里，与滚动内容分离 | 长字段列表滚动时按钮位置不动 |
| 900 | 同上 | 同上 |
| 640 | 同上；页头 banner/摘要与页签换行，按钮仍在底部动作栏 | 侧栏覆盖模式下页面本身不横向滚动 |

危险操作确认层：宽 `min(620, 0.92×视口)`、高 `≤0.92×视口`；只有正文滚动，取消/确认固定在页脚。
`danger_dialog_keeps_its_actions_visible_in_a_short_narrow_window` 在 640×640 下断言面板高度 ≤ 0.92×640+1、
确认与取消按钮底边不越出窗口、正文高度小于面板高度。

## 4. 双主题

结构页没有自己的颜色常量：表头、行、状态块、危险条都用 `Palette` 的语义色（`chrome / line / line_soft / hover / danger_soft / warn_soft`），
含义与 `foundation/theme-density-matrix.md` 的两套色板一致。本任务未新增裸色值，因此浅/深色切换不需要本页单独分支。

headless 证据：`structure_lists_follow_density_in_both_themes` 在 **浅色/深色 × 紧凑/标准** 四种组合下各开一个 900×900 窗口，
断言行高与表头高都等于当前密度的 `row_h`（26 / 32），且主操作按钮始终在位——即"主题只换配色，几何只随密度变"。
`token_tests::style_literals_follow_design_tokens`（全量测试中通过）继续保证本文件没有裸字号/裸圆角。

该用例是 **headless 布局级**证据，不是像素截图：它不能证明两套主题的实际渲染色块；像素证据由 Delivery 采集。
