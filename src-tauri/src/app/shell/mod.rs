//! 应用外壳：布局、关闭守卫、六类菜单和 GPUI 视图。

mod close;
mod jobs;
mod layout;
mod menus;
mod model;
mod view;

pub use model::WorkbenchModel;
pub use view::Workbench;

#[cfg(test)]
mod tests;
