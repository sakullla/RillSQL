# Windows 发布验证与渲染内存

## 问题与范围

main 提交 `ca3b3d9` 的 [CI 43592652443](https://github.com/sakullla/RillSQL/actions/runs/43592652443) 在 Windows Rust 测试阶段以 `0xc0000409` 退出，阻塞发布前置检查；同次 macOS、Linux、发布元数据检查成功。

开启原始输出后，[CI 43609591800](https://github.com/sakullla/RillSQL/actions/runs/43609591800) 确认直接原因：`density_theme_and_language_reach_the_rendered_chrome` 切换页面会启动真实 Tokio 读取与设置写盘，却没有允许后台唤醒，也没有等待读取回执就结束测试。Tokio 随后唤醒已经结束的 GPUI 测试调度器，触发非确定性检查及 `local task dropped by a thread that didn't spawn it` 二次 panic，Windows 以 `0xc0000409` 中止；Linux 同次在该测试报告非确定性失败。

修复在该集成测试中允许真实线程唤醒，使用既有 30 秒有界条件等待，确认连接/历史任务已完成、待发队列为空且设置写盘成功后再清理目录。未放宽纯模型测试的确定性要求。

另外，未优化 GPUI 元素构建确有过大的栈帧；缩小线程栈后能复现关系图栈溢出，并验证优化后通过。该性能问题与上述 CI 退出原因分别验证。

修复在已有栈帧拆分基础上覆盖连接弹窗、历史、设置、关系图、对比、交换、Redis、MongoDB、结构和记录视图。共享 `kit::stack_frame` 仅在调试构建禁止内联；发布构建允许内联。两个大型异步集成测试拆成阶段，保留原有断言。

## 单函数栈帧测量

Windows x64，Rust 1.100.0，`CARGO_INCREMENTAL=0`，未优化测试构建。读取 PE `.pdata` / unwind 信息计算栈分配，使用同次 PDB 解析函数名；比较 `ca3b3d9` 与本次实现。下表取对应函数及其拆分闭包中的最大单帧。

| 渲染区域 | 修改前 / bytes | 修改后 / bytes | 降幅 |
| --- | ---: | ---: | ---: |
| 关系图 | 685128 | 285128 | 58.4% |
| 历史页 | 598248 | 176552 | 70.5% |
| 连接弹窗 | 513032 | 137416 | 73.2% |
| 对比页 | 530856 | 96872 | 81.8% |
| 数据交换 | 494680 | 184168 | 62.8% |
| MongoDB | 476776 | 161528 | 66.1% |
| 管理页 | 401480 | 115144 | 71.3% |
| 结构表单 | 347880 | 158344 | 54.5% |

这些数字是单个函数的栈帧大小，不是整条调用链的峰值、每帧堆分配、进程内存或 FPS。

## 重绘分配

- Redis 详情、对比报告、关系图保存为不可变 `Arc` 快照；重绘只复制共享引用，不再深拷贝完整响应或 SQL 脚本。
- 关系图布局借用表与列信息，递归路径借用名称；外键列使用 `(表名, 列名)` 集合查询，避免逐列拼接字符串及线性查找，也区分名称中的点号。
- 历史页迭代筛选结果，只读取前 200 条用于构建列表，不再先收集全部结果并复制整行 SQL。状态判断找到第一条匹配项即停止；筛选、顺序、跨字段搜索语义保留。

## 验证

```powershell
$env:CARGO_INCREMENTAL = '0'
cargo +1.100.0 test --manifest-path src-tauri/Cargo.toml --locked
cargo +1.100.0 clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked
node .github/scripts/release.mjs check
node --test .github/scripts/release.test.mjs
```

本地全量 Rust 测试：1065 项库测试通过，7 项既有测试忽略，另 1 项打包辅助测试通过。发布元数据与 12 项发布保护测试通过。Clippy 通过，仍有仓库既有告警。

受限栈对照：同一关系图测试，设置 `RUST_MIN_STACK=786432`（768 KiB），旧版发生 stack overflow（Windows 退出码 `0xc00000fd`），修改版通过。512 KiB 下两版均溢出；这里没有声称支持任意小的线程栈。Windows CI 新增以下回归检查：

```powershell
$env:RUSTUP_TOOLCHAIN = '1.100.0'
pwsh -File .github/scripts/test-render-stack.ps1
```

脚本先用 Cargo 定位已编译的测试二进制，再仅给测试进程设置栈预算，避免让 rustc 编译线程也继承受限栈。

CI 同时开启测试原始输出与回溯，避免原生异常终止时丢失捕获中的诊断信息。远端三平台结果以 [PR #5](https://github.com/sakullla/RillSQL/pull/5) 的最新 CI 为准；未修改发布 tag 或安装包。
