# 1360 / 900 / 640 × 双主题 × 三档密度（T7）

证据类型：**headless 布局级 + token 级**（不是原生像素截图，见 `README.md`）。
每个宽度单独开一个 720 高的 GPUI 测试窗口（`WindowOptions { window_bounds: WindowBounds::Windowed(width × 720) }`），
窗口里放真实的 `GridPane`；密度通过 `Metrics::publish`、主题通过 `kit::apply_theme` 切换，
都是外壳运行时真正使用的同一条路径。

## 采集方式

- 宽度：`grid_geometry_comes_from_tokens_and_stays_inside_three_widths` 在 640/900/1360 各开一个窗口，
  种入 6 列 × 8 行并把每列宽度写成 260px（列宽合计 1560px），渲染后读 `#grid-body` 的真实边界。
- 密度：同一用例先断言 token 层（`row_height` / `cell_padding` 三档值），
  `density_and_theme_reach_the_rendered_grid` 再断言密度发布会话后 delegate 的 `cell_pad` 实际变化。
- 主题：`density_and_theme_reach_the_rendered_grid` 切到暗色再切回浅色，断言 `Palette::of(cx)` 的
  选中色与表头色等于 `Palette::dark()` / `Palette::light()` 对应字段——网格每帧读的就是这一份色板。
- 三态与键盘在同一批用例里按三档宽度渲染，见 `test-receipts.md` 的用例清单。

## 逐宽度结果（横向收口）

| 检查项 | 1360 | 900 | 640 |
| --- | --- | --- | --- |
| 列宽合计 | 1560px | 1560px | 1560px |
| `#grid-body` 实测宽度 | **1360px** | **900px** | **640px** |
| 结论 | 正文等于窗口宽，横向滚动留在表格内部 | 同左 | 同左 |

## 密度 token 值

| 密度 | `Metrics::row_h`（正文行高 = 表头高） | `cell_padding` 左右 | 骨架占位行高 |
| --- | --- | --- | --- |
| 紧凑 `Metrics::compact()` | 26px | 5px（`pane_pad` 10/2） | 16px（`row_h - 10`） |
| 标准 `Metrics::standard()` | 32px | 6px（12/2） | 22px |
| 宽松 `Metrics::relaxed()` | 40px | 8px（16/2） | 30px |

上下内边距固定为 0：行高已经决定行距，垂直居中由表格自己的 `items_center` 完成，不再叠一层裸间距。

## 双主题取色（网格实际使用的字段）

| 字段 | 浅色 `Palette::light()` | 深色 `Palette::dark()` | 用在哪 |
| --- | --- | --- | --- |
| `selection` | `#4f5bd5` @ 14% | `#7c86f5` @ 22% | 选中格底色（`render_td`） |
| `chrome` | `#f6f7f9` | `#1d1f24` | 表头底色（经 `kit::apply_theme` 写入 `table_head`） |
| `danger` / `danger_soft` | `#b42f2f` / `#fdecec` | `#f06d6d` / 深色软底 | 错误条与错误态、删除标记 |
| `accent` | `#4f5bd5` | 深色强调色 | 键盘焦点环（2px） |

## 三态与键盘（三档宽度下同一份结果）

| 检查项 | 结论 | 用例 |
| --- | --- | --- |
| 加载中画骨架（`#grid-loading`），不画空表 | 通过 | `loading_outranks_emptiness_on_first_open` |
| 无命中 → `no-match-state` + 清除条件 | 通过 | `empty_grids_use_the_kit_states_and_their_own_next_step` |
| 表里没有行 → `empty-state` + 插入行 | 通过 | 同上 |
| 查询没有返回行 → `empty-state`，无插入入口 | 通过 | 同上 |
| 首读失败 → `error-state` + `grid-retry` | 通过 | `a_first_read_failure_uses_the_kit_error_state_with_retry` |
| 有旧结果时失败 → 只一条 `#grid-read-error` | 通过 | `a_failed_read_keeps_the_sheet_and_shows_one_error_entry` |
| PageUp/PageDown/Tab 移动选区、滚动跟随、焦点环 | 通过 | `page_and_tab_keys_move_the_selection_and_paint_the_focus_ring` |

## 未取得项

- 原生 1360/900/640 像素截图、以及真实数据库连接下的三档宽度手点走查：本轮没有原生窗口与截屏通道授权，
  未生成 PNG，也不用历史截图替代。交付阶段在最终集成 candidate 上采集。
- 宽松密度的原生走查：`Metrics::relaxed()` 与外壳走同一条 `Metrics::publish` 路径
  （`app/shell/view.rs` 发布 `density.metrics()`），本轮在无头窗口里验证了 token 与 delegate 的生效值
  （行高 40 / 内边距 8），但没有在原生窗口里用设置页把密度切到宽松再截图；设置页的密度入口属 T8 范围。

## 表格流程在三档宽度下的走查（本轮补齐，headless 布局级）

`grid_flow_runs_at_three_widths_in_both_themes` 在 1360 / 900 / 640 三个宽度上各开一个 720 高的测试窗口，
每个宽度再跑「浅色 / 深色」x「紧凑 / 标准」四种组合，每次都在同一个 `GridPane` 上做完整一遍流程：

| 步骤 | 断言 | 三档宽度结论 |
| --- | --- | --- |
| 普通单击 (0,0) 再 Ctrl/Cmd 单击 (1,2) | `selection_count() == 2`、`has_extra_regions()` | 通过 |
| 多格复制 | `copy_text()` 以列名行 `id<TAB>` 开头（含表头） | 通过 |
| 记录面板 | `#grid-record` 与 `#grid-record-value-1` 渲染存在 | 通过 |
| 密度 | delegate 的 `cell_pad` 等于 `cell_padding(metrics)`（紧凑 5px / 标准 6px） | 通过 |
| 正文几何 | `#grid-body` 渲染存在且宽度不超过窗口（详见上一节的三档实测） | 通过 |
| 双击分隔柄 | `fit_col` 量出宽度写回会话（非行号列） | 通过 |

主题在这里只换颜色不动几何：同一用例先 `kit::apply_theme(dark)` 再渲染，选择色/表头色来自 `Palette::dark()`，
行高与内边距仍只由 `Metrics` 决定，所以四种组合的几何断言是同一组值。

## 未取得项（补充）

- 原生窗口里 1360/900/640 的手点走查与截图：没有原生窗口与截屏通道授权，见 `README.md` 与 `test-receipts.md`。
- 宽松密度（40px 行高）在这一轮的三档走查里没有逐一跑（用的是紧凑与标准两档，覆盖外壳默认与最常见选择），
  宽松密度的 token 值仍在上一节的表里，并有一条独立的 token 级用例。
