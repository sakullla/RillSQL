/** 后端错误码文案域（T6-A2/R6）：key 与 src-tauri error_codes.rs 的稳定码一一对应。 */
export const errors = {
  conn: {
    stage_failed: {
      message: "连接失败",
      hint: "请查看各阶段结果定位失败原因。",
    },
    auth_failed: {
      message: "数据库认证未通过",
      hint: "请检查数据库用户名和密码。",
    },
    redis_auth_failed: {
      message: "Redis 认证未通过",
      hint: "请检查 Redis 密码或 ACL 用户名。",
    },
    network_failed: {
      message: "网络连接失败",
      hint: "没有改用加密连接重试。",
    },
    tls_terminated: {
      message: "连接已终止",
      hint: "没有降级为不加密或不验证后重试。",
    },
    tls_downgrade_refused: {
      message: "TLS 校验未通过",
      hint: "没有改用不验证的连接。",
    },
    tls_ca_missing: {
      message: "缺少 CA 证书文件",
      hint: "请填写 CA 证书文件路径。",
    },
    tls_ca_unreadable: {
      message: "读不到 CA 证书文件",
      hint: "请确认文件路径与读取权限。",
    },
    tls_cert_missing: {
      message: "缺少客户端证书或私钥",
      hint: "客户端证书和私钥需要同时填写。",
    },
    tls_cert_unreadable: {
      message: "读不到客户端证书或私钥",
      hint: "请确认文件路径与读取权限。",
    },
    tls_not_configured: {
      message: "TLS 配置不完整",
      hint: "请补全加密设置后再试。",
    },
    version_read_failed: {
      message: "没有读到数据库版本",
      hint: "认证已通过，版本读取失败。",
    },
    schema_read_failed: {
      message: "没有读到表结构清单",
      hint: "文件已打开，但结构清单读取失败。",
    },
    sqlite_file_missing: {
      message: "请选择数据库文件",
      hint: "SQLite 只打开已有文件。",
    },
    sqlite_open_failed: {
      message: "SQLite 文件打开失败",
      hint: "没有打开文件，后续阶段不会执行。",
    },
    foreign_keys_read_failed: {
      message: "没有读到 foreign_keys 状态",
      hint: "文件已打开，但外键开关读取失败。",
    },
    ssh_auth_failed: {
      message: "登录 SSH 跳板机失败",
      hint: "请检查 SSH 用户、私钥或口令。",
    },
    ssh_key_unreadable: {
      message: "读不到或无法解锁私钥",
      hint: "私钥口令只用于解锁，没有写入配置。",
    },
    ssh_not_configured: {
      message: "缺少 SSH 配置",
      hint: "未执行。",
    },
    tunnel_failed: {
      message: "SSH 通道未打通",
      hint: "已登录跳板机，但到数据库的通道没有打通。",
    },
    tunnel_not_ready: {
      message: "隧道没有就绪",
      hint: "尚未连接数据库。",
    },
    host_key_changed: {
      message: "SSH 主机密钥已变化",
      hint: "已阻止连接，没有发送任何凭据。请先核实新指纹，再忘记已保存的密钥重新测试。",
    },
    host_key_inconsistent: {
      message: "SSH 主机密钥状态不一致",
      hint: "没有继续认证。",
    },
    route_resolve_failed: {
      message: "连接路径解析失败",
      hint: "证书仍按数据库主机名校验，没有改用转发器地址。",
    },
    password_required: {
      message: "请填写数据库密码",
      hint: "凭据库没有可用密码，本次也没有输入。没有把空密码写入配置。",
    },
    sqlite_not_remote: {
      message: "SQLite 不走远程连接",
      hint: "未执行。",
    },
    not_started: {
      message: "连接没有开始",
      hint: "未执行。",
    },
    probe_failed: {
      message: "探活没有通过",
      hint: "连接没有建立为可用会话。",
    },
    version_unsupported: {
      message: "数据库版本不受支持",
      hint: "方言按受支持版本设计，低于下限的连接在测试阶段即停止。",
    },
    unknown_version: {
      message: "无法识别数据库版本",
      hint: "请确认连接指向受支持的数据库。",
    },
  },
  ssh: {
    host_key_wait: {
      message: "等待确认 SSH 服务器指纹",
      hint: "请通过可信渠道核对指纹后再继续。",
    },
  },
  import: {
    passphrase_required: {
      message: "这份文件带口令加密，请输入导出时设置的口令。",
      hint: "",
    },
    bad_passphrase: {
      message: "口令不正确或文件已损坏，已整体拒绝导入。",
      hint: "",
    },
    unrecognized: {
      message: "无法识别这份文件：不是有效的 RillSQL 连接导出。",
      hint: "",
    },
  },
  dump: {
    engine_unsupported: {
      message: "该引擎没有整库导出；请使用对应的浏览器访问。",
      hint: "",
    },
    connection_mismatch: {
      message: "连接类型和引擎不一致。",
      hint: "",
    },
    session_gap: {
      message: "当前连接打开的是别的数据库，没有另开连接读取。",
      hint: "",
    },
    session_gap_main: {
      message: "当前连接打开的是 main，没有另开连接读取其它库。",
      hint: "",
    },
    database_required: {
      message: "需要数据库名，没有另开连接。",
      hint: "",
    },
  },
  migrate: {
    not_queryable: {
      message: "这个连接没有连着可查询的数据库。",
      hint: "",
    },
    redis_not_target: {
      message: "Redis 不是迁移目标；数据经键命令写入。",
      hint: "",
    },
    mongo_not_target: {
      message: "MongoDB 不是迁移目标；文档经集合浏览器写入。",
      hint: "",
    },
    target_not_table: {
      message: "目标必须是已经存在的表；视图不能作为迁移目标。",
      hint: "",
    },
    no_mapped_columns: {
      message: "没有可迁移的列：两边的列名都对不上，请在列映射里手动指定。",
      hint: "",
    },
    duplicate_mapping: {
      message: "有两个源列映射到了同一个目标列。",
      hint: "",
    },
    source_read_failed: {
      message: "读取源表失败。",
      hint: "",
    },
    target_read_failed: {
      message: "读取目标表失败。",
      hint: "",
    },
    table_missing: {
      message: "目标表不存在。",
      hint: "",
    },
  },
};
