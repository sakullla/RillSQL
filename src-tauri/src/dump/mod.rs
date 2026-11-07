//! 全库导出（R7）：依赖序 DDL + 流式数据 INSERT。
//! order 是与 compare 共享的排序核心；dump 生成单个 `.sql` 文件，进度走 `dump_progress` 事件，可取消。

pub mod commands;
#[allow(clippy::module_inception)]
pub(crate) mod dump;
pub(crate) mod order;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&commands::database_dump::<tauri::Wry>),
        std::any::type_name_of_val(&commands::database_dump_cancel),
        std::any::type_name_of_val(&dump::run_dump),
        std::any::type_name::<dump::DumpRequest>(),
    );
    "dump"
}
