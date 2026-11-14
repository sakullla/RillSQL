# 连接分组与对象浏览器（T4）验证目录

- 日期：2026-11-14
- 任务：T4「实现可持久化连接分组，并将对象浏览器改造成支持懒加载、筛选、键盘和大规模的虚拟化树」
- 覆盖需求：R3（密集列表/键盘/右键）、R4（三态、密度、三档宽度）、R6（对象浏览端到端）、R7（分组与规模）
- 覆盖规范：S5.2（Listbox 行与选中底）、S6.3（对象树与右键）、S8.5（无数据/无命中/错误语义）、S9 / S12.2（密度 token）、S12.4（三态）
- 改动范围：
  - `src-tauri/src/connections/{groups.rs,store.rs,service.rs,mod.rs}`：分组模型、迁移、Store 命令、Service 入口
  - `src-tauri/src/catalog/hierarchy.rs`：`visible_under` 在筛选时只走已加载节点并保留祖先
  - `src-tauri/src/app/shell/{explorer.rs,model.rs,jobs.rs,menus.rs,chrome.rs,view.rs}`：扁平可见行、视口窗口、分组命令、拖拽/右键归组、分组输入面板与删除确认
  - `src-tauri/src/i18n/mod.rs`：只追加 `ui.group.*` 文案键
  - `src-tauri/src/app/shell/tests.rs`、`src-tauri/src/connections/{tests.rs,groups.rs}`：新增用例

## 文件

| 文件 | 内容 |
| --- | --- |
| `group-restart.md` | 分组创建/重命名/折叠/删除、删除后连接回顶层、迁移与重启恢复的断言与数据路径 |
| `virtualization-and-scale.md` | 视口窗口算法、每行高度与元素数量、数百节点与三档宽度的实测几何 |
| `interactions.md` | 长名称、展开箭头、状态、空库/无命中/错误、键盘导航、对象右键与拖拽 |
| `test-receipts.md` | 本轮实际运行的命令、结果与用例清单 |

## 证据类型（请勿混读）

- **源码级 + 模型断言**：分组落库（真实 SQLite 配置库）、成员关系、折叠状态、筛选只读已加载节点、键盘与命令路由。
- **headless 布局级**：GPUI 测试窗口里读出的元素边界与行高（1360/900/640 × 800）、行元素创建数量、滚动到边界后的元素回收。
- **未取得：原生像素截图**。本任务运行环境没有原生窗口与截屏通道，本目录不用历史截图替代，也没有把 headless 边界当像素证据。三档宽度与双主题双密度的原生截图、几百节点下的真实交互手感由 Delivery Owner 在最终集成 candidate 上取得。
- **未取得：远端数据库 fixture 的多库/多模式端到端**。PostgreSQL/MySQL 端到端证据需要 docker fixture，本轮以模型级与 SQLite 真实会话证据覆盖，原因见 `interactions.md` 的「未解决事实」。

## 与设计基准的关系

对象浏览器在 S6.3 里是“左侧层级 + 右键动作”的密集树；本轮只改可见行的构建方式（分组标题 + 虚拟化窗口）与分组命令，不新增第二套视觉语言：行高继续取 `Metrics::row_h / tree_row_h`，显示字色、选中底、hover 与状态点全部沿用 `kit`。
