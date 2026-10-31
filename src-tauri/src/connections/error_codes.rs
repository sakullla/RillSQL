//! 面向用户的错误码注册表（T6-A2/R6）。
//!
//! 目标：后端错误结构携带稳定短码（如 `conn.auth_failed`），前端在呈现处先查
//! `code → i18n errors 域 key`，命中则显示翻译文案；无 code 或未知 code 回退原文。
//! 中文 message 全部保持不变（后端单测断言不变）；code 一经发布不得改动语义。
//!
//! 约定：
//! - code 形如 `域.名称`，全小写、下划线分词（`^[a-z0-9]+(\.[a-z0-9_]+)+$`）。
//! - 只给「会经 UI 呈现」的错误类型赋码；纯内部错误不在此列。

/// 校验 code 形状；错误码稳定性单测用它保证命名规范不被破坏。
pub fn is_valid_code(code: &str) -> bool {
    let mut parts = code.split('.');
    let head = parts.next().unwrap_or_default();
    if head.is_empty() || !head.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return false;
    }
    let mut count = 1;
    for part in parts {
        count += 1;
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return false;
        }
    }
    count >= 2
}

// ---- 连接测试 / 打开（StageError::Fail 的 code）----
pub const CONN_STAGE_FAILED: &str = "conn.stage_failed";
pub const CONN_AUTH_FAILED: &str = "conn.auth_failed";
pub const CONN_REDIS_AUTH_FAILED: &str = "conn.redis_auth_failed";
pub const CONN_NETWORK_FAILED: &str = "conn.network_failed";
pub const CONN_TLS_TERMINATED: &str = "conn.tls_terminated";
pub const CONN_TLS_DOWNGRADE_REFUSED: &str = "conn.tls_downgrade_refused";
pub const CONN_TLS_CA_MISSING: &str = "conn.tls_ca_missing";
pub const CONN_TLS_CA_UNREADABLE: &str = "conn.tls_ca_unreadable";
pub const CONN_TLS_CERT_MISSING: &str = "conn.tls_cert_missing";
pub const CONN_TLS_CERT_UNREADABLE: &str = "conn.tls_cert_unreadable";
pub const CONN_TLS_NOT_CONFIGURED: &str = "conn.tls_not_configured";
pub const CONN_VERSION_READ_FAILED: &str = "conn.version_read_failed";
pub const CONN_SCHEMA_READ_FAILED: &str = "conn.schema_read_failed";
pub const CONN_SQLITE_FILE_MISSING: &str = "conn.sqlite_file_missing";
pub const CONN_SQLITE_OPEN_FAILED: &str = "conn.sqlite_open_failed";
pub const CONN_FOREIGN_KEYS_READ_FAILED: &str = "conn.foreign_keys_read_failed";
pub const CONN_SSH_AUTH_FAILED: &str = "conn.ssh_auth_failed";
pub const CONN_SSH_KEY_UNREADABLE: &str = "conn.ssh_key_unreadable";
pub const CONN_SSH_NOT_CONFIGURED: &str = "conn.ssh_not_configured";
pub const CONN_TUNNEL_FAILED: &str = "conn.tunnel_failed";
pub const CONN_TUNNEL_NOT_READY: &str = "conn.tunnel_not_ready";
pub const CONN_HOST_KEY_CHANGED: &str = "conn.host_key_changed";
pub const CONN_HOST_KEY_INCONSISTENT: &str = "conn.host_key_inconsistent";
pub const CONN_ROUTE_RESOLVE_FAILED: &str = "conn.route_resolve_failed";
pub const CONN_PASSWORD_REQUIRED: &str = "conn.password_required";
pub const CONN_SQLITE_NOT_REMOTE: &str = "conn.sqlite_not_remote";
pub const CONN_NOT_STARTED: &str = "conn.not_started";
pub const CONN_PROBE_FAILED: &str = "conn.probe_failed";
pub const CONN_VERSION_UNSUPPORTED: &str = "conn.version_unsupported";
/// 预留码（当前无生产赋码点，前端字典已备）：驱动返回无法归类的版本串时使用。
#[allow(dead_code)]
pub const CONN_UNKNOWN_VERSION: &str = "conn.unknown_version";
pub const SSH_HOST_KEY_WAIT: &str = "ssh.host_key_wait";

// ---- 连接导入（RillSQL 导出文件）----
pub const IMPORT_PASSPHRASE_REQUIRED: &str = "import.passphrase_required";
pub const IMPORT_BAD_PASSPHRASE: &str = "import.bad_passphrase";
pub const IMPORT_UNRECOGNIZED: &str = "import.unrecognized";

// ---- 全库导出（dump）----
pub const DUMP_ENGINE_UNSUPPORTED: &str = "dump.engine_unsupported";
pub const DUMP_CONNECTION_MISMATCH: &str = "dump.connection_mismatch";
pub const DUMP_SESSION_GAP: &str = "dump.session_gap";
pub const DUMP_SESSION_GAP_MAIN: &str = "dump.session_gap_main";
pub const DUMP_DATABASE_REQUIRED: &str = "dump.database_required";

// ---- 跨库迁移（migrate）----
pub const MIGRATE_NOT_QUERYABLE: &str = "migrate.not_queryable";
pub const MIGRATE_REDIS_NOT_TARGET: &str = "migrate.redis_not_target";
pub const MIGRATE_MONGO_NOT_TARGET: &str = "migrate.mongo_not_target";
pub const MIGRATE_TARGET_NOT_TABLE: &str = "migrate.target_not_table";
pub const MIGRATE_NO_MAPPED_COLUMNS: &str = "migrate.no_mapped_columns";
pub const MIGRATE_DUPLICATE_MAPPING: &str = "migrate.duplicate_mapping";
pub const MIGRATE_SOURCE_READ_FAILED: &str = "migrate.source_read_failed";
pub const MIGRATE_TARGET_READ_FAILED: &str = "migrate.target_read_failed";
/// 预留码（当前无生产赋码点，前端字典已备）：目标表在校验后被删除等场景。
#[allow(dead_code)]
pub const MIGRATE_TABLE_MISSING: &str = "migrate.table_missing";

/// 给纯字符串错误附码的前缀约定：`[code] 原文`。
/// 前端 `parseCodedError` 识别该前缀并剥离后回退正文；后端单测用 `contains` 断言不受影响。
pub fn coded_string(code: &str, message: impl Into<String>) -> String {
    format!("[{code}] {}", message.into())
}

/// 带插值参数的附码前缀：`[code key="value"] 原文`（T6-A3，T6-A4 修订）。
/// 供前端在非 zh-CN locale 用字典条目插值（如 dump.session_gap 的 {current}/{database}）；
/// 参数值统一双引号包裹，值中的 `\` 与 `"` 强制转义为 `\\` 与 `\"`（release 同样生效），
/// 保证前缀始终可被前端识别剥离、含空格/引号的值（如库名 "My Database"）不截断。
pub fn coded_string_params(code: &str, params: &[(&str, &str)], message: impl Into<String>) -> String {
    debug_assert!(params.iter().all(|(key, _)| is_valid_param_key(key)), "参数名须为小写下划线标识符");
    let joined = params
        .iter()
        .map(|(key, value)| format!("{key}=\"{}\"", escape_param_value(value)))
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        return coded_string(code, message);
    }
    format!("[{code} {joined}] {}", message.into())
}

/// 参数值转义约定（与前端 parseCodedText 的 unescapeParamValue 对应）：`\` → `\\`，`"` → `\"`。
fn escape_param_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn is_valid_param_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
