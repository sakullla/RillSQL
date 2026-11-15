//! 应用外壳：布局、关闭守卫、六类菜单和 GPUI 视图。

mod chrome;
mod close;
mod conn_modal;
mod conn_page;
mod danger;
pub(crate) mod explorer;
mod jobs;
pub(crate) mod layout;
mod menus;
mod model;
pub(crate) mod nav;
mod update;
mod view;

pub use close::{update_block_key, CloseFacts};
pub use model::{WorkItemKind, WorkbenchModel};
pub use view::Workbench;

#[cfg(test)]
mod tests;
