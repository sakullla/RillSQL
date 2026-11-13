# 对象比较页（T11）验证材料

范围：`src-tauri/src/app/views/compare/`。目标是统一加载、错误、空态与窄宽布局，
保持比较、取消、脚本放入编辑器的既有行为（脚本仍不执行）。

| 文件 | 内容 |
| --- | --- |
| [compare-audit.md](compare-audit.md) | 改动前的偏差清单、逐条处置与仍未覆盖的事实 |
| [widths-themes-densities.md](widths-themes-densities.md) | 1360/900/640 × 浅深色 × 紧凑标准 的重排实测（GPUI 无头窗口，bounds 证据） |
| [test-receipts.md](test-receipts.md) | 本轮命令、通过/失败/ignored 计数、未取得项 |

本轮没有原生窗口截图：本机没有可自动化的桌面会话，见 `widths-themes-densities.md` 的
“证据边界”一节，截图入口留给 Delivery Owner 在集成 candidate 上补齐。
