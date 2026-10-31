//! 跨库数据迁移（R8）：源端流式分页读，目标端分批事务写入。
//! 与 compare/data.rs 不同通路：这里允许两侧引擎不同、逐页读不整表进内存、单行失败记录主键继续。
//! 进度走 `migrate_progress` 事件，取消用 AtomicBool 注册表（dump/grid 同一模式）。

pub mod commands;
pub(crate) mod migrate;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&commands::migrate_data::<tauri::Wry>),
        std::any::type_name_of_val(&commands::migrate_cancel),
        std::any::type_name_of_val(&migrate::run_migrate),
        std::any::type_name::<migrate::MigrateRequest>(),
    );
    "migrate"
}
