//! 连接：保存、测试、打开和删除，TLS 校验名与 SSH 通道分开。

mod commands;
mod drivers;
mod forward;
mod hostkey;
mod model;
mod route;
mod secrets;
mod service;
mod stages;
mod store;
mod verify;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name::<commands::ConnectionsState>(),
        std::any::type_name_of_val(&commands::ConnectionsState::new),
        std::any::type_name_of_val(&commands::connections_list),
        std::any::type_name_of_val(&commands::connections_save),
        std::any::type_name_of_val(&commands::connections_delete),
        std::any::type_name_of_val(&commands::connections_test),
        std::any::type_name_of_val(&commands::connections_open),
        std::any::type_name_of_val(&commands::connections_disconnect),
        std::any::type_name_of_val(&commands::connections_confirm_host_key),
        std::any::type_name_of_val(&commands::connections_forget_host_key),
    );
    "connections"
}

#[cfg(test)]
mod tests;
