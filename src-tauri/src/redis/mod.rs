//! Redis 键浏览器（R11）：SCAN 分页、类型化值读取、TTL 修改、删除/重命名与命令执行通道。
//! 键空间浏览一律走 SCAN 游标（禁 KEYS）；写删类操作由前端确认后携带 confirmed 标记执行。

pub mod commands;
pub(crate) mod exec;
pub(crate) mod keys;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name_of_val(&commands::redis_scan_keys),
        std::any::type_name_of_val(&commands::redis_read_key),
        std::any::type_name_of_val(&exec::run_command),
        std::any::type_name::<keys::ScanPage>(),
    );
    "redis"
}

#[cfg(test)]
mod tests;
