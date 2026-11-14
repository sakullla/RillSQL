# 虚拟化、元素数量与规模

## 可见行的构建

资源管理器的滚动容器里不再逐个 child 追加所有可见行，而是：

1. `WorkbenchModel::explorer_rows()` 把“分组标题 → 组内连接 → 未分组连接 → 每条连接的树行”拍成一条 `Vec<ExplorerItem>`；
2. `explorer_heights()` 给出每行的真实高度（分组/树行 = `Metrics::tree_row_h`，连接行与目录错误行 = `Metrics::row_h`）；
3. `explorer::window_rows(heights, scroll_top, viewport_h, overscan=4)` 按累加高度算出视口内的行区间与上下空白；
4. 渲染只创建该区间内的行元素，上下各用一个固定高度的空白 `div` 撑起剩余滚动高度。

滚动位置读 `ScrollHandle::offset().y`（gpui 向下滚为负）与 `ScrollHandle::bounds().size.height`（视口高）。滚动时 gpui 会 `cx.notify` 当前视图，因此窗口随滚动重算；视口未知（第一帧为 0）时按 480px 兜底，绝不退化成渲染全部行。

## 元素数量

用例 `app::shell::explorer::tests::element_count_does_not_grow_with_total_rows`：20 / 200 / 5000 / 100000 行、行高 23、视口 230、overscan 4，创建元素数 `<= 10 + 2*4`。

用例 `app::shell::tests::explorer_ids_are_stable_and_dense_under_hundreds_of_connections`：300 条连接 + 1 个分组 → 301 条可见行；同一序列里 `item.id()` 唯一；视口 480 时创建的窗口长度与 20 条连接时相差不超过 4。

## headless 实测（1360×800，紧凑密度）

| 指标 | 数值 | 来源 |
| --- | --- | --- |
| 列表视口高 | 670 px | `ScrollHandle::bounds().size.height` |
| 内容总高 | 7831 px（301 行：1×23 + 300×26 + 8 pb） | `max_offset` 7161 + 视口 670 |
| 顶部创建的元素 | 29 行 | `hundreds_of_connections_render_only_the_visible_explorer_rows` |
| 滚到底后创建的元素 | 35 行（264..298），顶部行已回收 | 同上（`window.scroll` 到 -9000 后重绘） |
| 连接行高 | 26 px（`Metrics::row_h`） | 元素边界断言 |
| 分组标题行高 | 23 px（`Metrics::tree_row_h`） | 元素边界断言 |

行元素带 `flex_none()`：首版没有加，flex 会把 26px 的行压到 21px，内容总高比名义高度少 175px，滚到底仍差最后几行。这是本轮实测发现并修掉的问题，用例用元素边界（26 / 23）守住。

## 三档宽度（`object_browser_geometry_holds_at_1360_900_and_640`）

| 窗口宽 | 侧栏宽 | 列表内宽 | 侧栏模式 | 连接行高 | 12 条连接创建 |
| --- | --- | --- | --- | --- | --- |
| 1360 | 234 | 227 | 并排（docked） | 26 | 12 |
| 900 | 234 | 227 | 并排（docked） | 26 | 12 |
| 640 | 234 | 227 | 覆盖（overlay） | 26 | 12 |

断言同时覆盖：侧栏贴着活动栏（origin.x ≤ 64+1）、连接行与列表宽度不超过侧栏宽（无横向溢出）、`painted().sidebar_overlay` 只在 640 为 true。

## 与旧行为的差别

- 旧实现把每个连接的 `Vec<NavRow>` 全部做成 child，元素数量随展开节点线性增长；现在只与视口高相关。
- 跳转定义/键盘定位原先用 `scroll_to_item(下标)`（按下标数 child）；改为按行高前缀和算偏移后 `set_offset`，虚拟化后下标不再等于元素序号，仍能定位。
- 行 id 全部是稳定字符串：`group:<分组 id>`、`conn:<连接 id>`、`nav:<节点 id>`、`conn-error:<连接 id>`、`note:<连接 id>`、`catalog-error`、`explorer-no-match`。所有鼠标、键盘与右键命令都从这些 id 派生，不按下标索引内层 Vec。
