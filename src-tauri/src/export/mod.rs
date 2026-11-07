//! 导入、导出、全库导出和跨库迁移。工作簿编解码只在 xlsx 读写函数里初始化。

mod desk;
mod parse;
mod tasks;
mod write;
mod xlsx;

pub use desk::{commit_import_guarded, export_full, export_loaded, open_import, watch_dump, watch_migrate, CancelPoint, ImportEntry, ImportPreview};
pub use parse::{suggest_mapping, TableField};
pub use tasks::{prefer_cancel, Phase, TaskLog, TaskView};
pub use write::LoadedExport;
pub use xlsx::{read_xlsx, workbook_initializations, workbook_test_lock, write_xlsx};

#[cfg(test)]
mod tests;
