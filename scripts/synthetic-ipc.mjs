// Synthetic IPC for browser verification; never imported by the application.
export function installSyntheticIpc(initialTheme) {
  const settings = {
    theme: initialTheme,
    density: "compact",
    editorFont: 13,
    pageSize: 100,
    previewKB: 1,
    objectPage: 100,
    nullCopy: "NULL",
    typeToConfirm: true,
    confirmClose: true,
  };
  localStorage.setItem("rillsql.shell", JSON.stringify(settings));

  const object = (name, kind, estimatedRows) => ({ name, kind, estimatedRows, stale: false });
  const saved = [
    {
      id: "pg-dev",
      name: "书店 · 开发",
      engine: "postgres",
      env: "dev",
      host: "db.internal",
      port: 5432,
      database: "bookstore",
      username: "reader",
      filePath: null,
      tls: { mode: "verify", serverName: "db.internal", caMode: "system", caFile: null, clientCert: null, clientKey: null },
      ssh: { host: "bastion.internal", port: 22, username: "ops", auth: "key", keyPath: "C:\\Users\\local\\.ssh\\id_ed25519" },
      passwordSaved: true,
      sshPasswordSaved: false,
      sshHostKey: "SHA256:fixture-bastion",
    },
    {
      id: "mysql-stock",
      name: "库存 · 本地",
      engine: "mysql",
      env: "local",
      host: "127.0.0.1",
      port: 3306,
      database: "inventory",
      username: "root",
      filePath: null,
      tls: { mode: "off", serverName: "127.0.0.1", caMode: "system", caFile: null, clientCert: null, clientKey: null },
      ssh: null,
      passwordSaved: false,
      sshPasswordSaved: false,
      sshHostKey: null,
    },
    {
      id: "mssql-report",
      name: "报表 · SQL Server",
      engine: "sqlserver",
      env: "test",
      host: "sql.internal",
      port: 1433,
      database: "rillsql",
      username: "sa",
      filePath: null,
      tls: { mode: "require", serverName: "sql.internal", caMode: "system", caFile: null, clientCert: null, clientKey: null },
      ssh: null,
      passwordSaved: true,
      sshPasswordSaved: false,
      sshHostKey: null,
    },
    {
      id: "redis-cache",
      name: "缓存 · Redis",
      engine: "redis",
      env: "local",
      host: "127.0.0.1",
      port: 6379,
      database: "0",
      username: null,
      filePath: null,
      tls: { mode: "off", serverName: "127.0.0.1", caMode: "system", caFile: null, clientCert: null, clientKey: null },
      ssh: null,
      passwordSaved: true,
      sshPasswordSaved: false,
      sshHostKey: null,
    },
    {
      id: "mongo-docs",
      name: "文档 · MongoDB",
      engine: "mongodb",
      env: "local",
      host: "127.0.0.1",
      port: 27017,
      database: "admin",
      username: "root",
      filePath: null,
      replicaSet: null,
      tls: { mode: "off", serverName: "127.0.0.1", caMode: "system", caFile: null, clientCert: null, clientKey: null },
      ssh: null,
      passwordSaved: true,
      sshPasswordSaved: false,
      sshHostKey: null,
    },
    {
      id: "sqlite-notes",
      name: "笔记",
      engine: "sqlite",
      env: "local",
      host: null,
      port: null,
      database: null,
      username: null,
      filePath: "C:\\Users\\local\\notes.sqlite",
      tls: null,
      ssh: null,
      passwordSaved: false,
      sshPasswordSaved: false,
      sshHostKey: null,
    },
  ];
  const catalog = [
    {
      id: "pg-dev",
      name: "书店 · 开发",
      engine: "postgres",
      env: "dev",
      envLabel: "开发",
      envTone: "dev",
      status: "connected",
      defaultDatabase: "bookstore",
      foreignKeys: null,
      error: null,
      databases: [
        {
          name: "bookstore",
          error: null,
          schemas: [
            {
              name: "public",
              error: null,
              total: 4,
              objects: [object("customers", "table", 86), object("orders", "table", 128), object("order_items", "table", 340), object("order_totals", "view", null)],
            },
          ],
        },
      ],
    },
    {
      id: "mssql-report",
      name: "报表 · SQL Server",
      engine: "sqlserver",
      env: "test",
      envLabel: "测试",
      envTone: "test",
      status: "connected",
      defaultDatabase: "rillsql",
      foreignKeys: null,
      error: null,
      databases: [
        {
          name: "rillsql",
          error: null,
          schemas: [
            {
              name: "dbo",
              error: null,
              total: 3,
              objects: [object("customers", "table", 3), object("orders", "table", 3), object("paid_orders", "view", null), object("count_paid_orders", "procedure", null), object("note_seq", "sequence", null)],
            },
          ],
        },
      ],
    },
    {
      id: "redis-cache",
      name: "缓存 · Redis",
      engine: "redis",
      env: "local",
      envLabel: "本地",
      envTone: "local",
      status: "connected",
      defaultDatabase: "0",
      foreignKeys: null,
      error: null,
      // Redis 会话没有库/schema 层级：键空间经 redis_scan_keys 分页读取。
      databases: [],
    },
    {
      id: "mongo-docs",
      name: "文档 · MongoDB",
      engine: "mongodb",
      env: "local",
      envLabel: "本地",
      envTone: "local",
      status: "connected",
      defaultDatabase: "admin",
      foreignKeys: null,
      error: null,
      // 目录形状复用 MySQL 先例（databases/无 schema）；集合行估算就是文档计数。
      databases: [
        {
          name: "rillsql",
          error: null,
          schemas: [
            {
              name: "",
              error: null,
              total: 2,
              objects: [object("orders", "table", 25), object("customers", "table", 3)],
            },
          ],
        },
      ],
    },
    {
      id: "mysql-stock",
      name: "库存 · 本地",
      engine: "mysql",
      env: "local",
      envLabel: "本地",
      envTone: "local",
      status: "disconnected",
      defaultDatabase: "inventory",
      foreignKeys: null,
      error: null,
      databases: [],
    },
    {
      id: "sqlite-notes",
      name: "笔记",
      engine: "sqlite",
      env: "local",
      envLabel: "本地",
      envTone: "local",
      status: "connected",
      defaultDatabase: "main",
      foreignKeys: true,
      error: null,
      databases: [{ name: "main", error: null, schemas: [{ name: "", error: null, total: 1, objects: [object("notes", "table", 12)] }] }],
    },
  ];
  const orderColumns = [
    { name: "id", typeName: "integer", nullable: false, primary: true, unique: true, auto: true },
    { name: "customer", typeName: "text", nullable: true, primary: false, unique: false, auto: false },
    { name: "status", typeName: "text", nullable: false, primary: false, unique: false, auto: false },
    { name: "total", typeName: "numeric", nullable: false, primary: false, unique: false, auto: false },
    { name: "note", typeName: "text", nullable: true, primary: false, unique: false, auto: false },
    { name: "created_at", typeName: "timestamp", nullable: false, primary: false, unique: false, auto: false },
  ];
  const orderRows = [
    ["1001", "林夏", "paid", "128.00", null, "2026-10-01 09:12"],
    ["1002", "周宁", "pending", "56.50", "加急", "2026-10-02 14:40"],
    ["1003", null, "cancelled", "0.00", "客户取消", "2026-10-03 11:05"],
  ];
  const noteColumns = [
    { name: "id", typeName: "integer", nullable: false, primary: true, unique: true, auto: true },
    { name: "title", typeName: "text", nullable: false, primary: false, unique: false, auto: false },
    { name: "body", typeName: "text", nullable: true, primary: false, unique: false, auto: false },
  ];
  const noteRows = [
    ["1", "进货核对", "十月第二周的到货数量还差两箱。"],
    ["2", "会员价", null],
  ];
  function rowsOf(columns, matrix, key) {
    return matrix.map((values) => {
      const cells = {};
      columns.forEach((column, index) => {
        cells[column.name] = values[index];
      });
      return { id: String(values[0]), cells, located: true, locatorColumns: [key], locatorValues: [values[0]] };
    });
  }
  const sqlserverColumns = [
    { name: "id", typeName: "int", nullable: false, defaultSql: "", primary: true, auto: true, generated: false },
    { name: "customer_id", typeName: "int", nullable: false, defaultSql: "", primary: false, auto: false, generated: false },
    { name: "status", typeName: "nvarchar", nullable: false, defaultSql: "", primary: false, auto: false, generated: false },
    { name: "total", typeName: "decimal", nullable: false, defaultSql: "0", primary: false, auto: false, generated: false },
  ];
  const sqlserverRows = [
    ["1", "1", "paid", "128.50"],
    ["2", "1", "pending", "42.00"],
    ["3", "2", "paid", "76.20"],
  ];
  function schemaFor(table, engineHint) {
    if (engineHint === "sqlserver") {
      return {
        engine: "sqlserver",
        schema: "dbo",
        name: "orders",
        kind: "table",
        columns: sqlserverColumns,
        key: ["id"],
        rows: rowsOf(sqlserverColumns, sqlserverRows, "id"),
        sql: "SELECT * FROM [dbo].[orders] ORDER BY [id] ASC OFFSET 0 ROWS FETCH NEXT 100 ROWS ONLY",
      };
    }
    if (table === "notes") {
      return { engine: "sqlite", schema: "", name: "notes", kind: "table", columns: noteColumns, key: ["id"], rows: rowsOf(noteColumns, noteRows, "id"), sql: 'select "id", "title", "body" from "notes" limit 100' };
    }
    return { engine: "postgres", schema: "public", name: "orders", kind: "table", columns: orderColumns, key: ["id"], rows: rowsOf(orderColumns, orderRows, "id"), sql: 'select "id", "customer", "status", "total", "note", "created_at" from "public"."orders" limit 100' };
  }
  const history = [
    {
      id: "h1",
      sql: "select id, customer, status, total\nfrom orders\nwhere status <> 'cancelled';",
      connectionId: "pg-dev",
      connectionName: "书店 · 开发",
      databaseName: "bookstore",
      engine: "postgres",
      startedAt: "2026-10-14T02:10:00.000Z",
      finishedAt: "2026-10-14T02:10:00.018Z",
      status: "ok",
      elapsedMs: 18,
      summary: "3 行",
    },
    {
      id: "h2",
      sql: "update orders set status = 'paid' where id = 1002;",
      connectionId: "pg-dev",
      connectionName: "书店 · 开发",
      databaseName: "bookstore",
      engine: "postgres",
      startedAt: "2026-10-13T08:22:00.000Z",
      finishedAt: "2026-10-13T08:22:00.040Z",
      status: "error",
      elapsedMs: 40,
      summary: "权限不足",
    },
    {
      id: "h3",
      sql: "select * from legacy_audit;",
      connectionId: "gone-db",
      connectionName: "旧报表库",
      databaseName: "reports",
      engine: "postgres",
      startedAt: "2026-09-02T01:00:00.000Z",
      finishedAt: "2026-09-02T01:00:01.000Z",
      status: "ok",
      elapsedMs: 1000,
      summary: "12 行",
    },
  ];
  function testReport(connection) {
    if (connection.engine === "redis") {
      return {
        status: "ok",
        failStage: null,
        hint: null,
        summary: "Redis 7.4.1",
        fingerprint: null,
        algorithm: null,
        stages: [
          { kind: "network", label: "连接数据库服务器", status: "ok", message: "127.0.0.1:6379" },
          { kind: "database_authentication", label: "登录数据库", status: "ok", message: "密码认证" },
          { kind: "server_version", label: "读取数据库版本", status: "ok", message: "Redis 7.4.1" },
        ],
      };
    }
    if (connection.engine === "sqlite") {
      return {
        status: "ok",
        failStage: null,
        hint: null,
        summary: "SQLite 3.46.0",
        fingerprint: null,
        algorithm: null,
        stages: [
          { kind: "open_file", label: "打开文件", status: "ok", message: connection.filePath },
          { kind: "read_schema", label: "读取表结构清单", status: "ok", message: "1 张表" },
          { kind: "foreign_keys", label: "检查外键设置", status: "ok", message: "ON" },
        ],
      };
    }
    return {
      status: "ok",
      failStage: null,
      hint: null,
      summary: "PostgreSQL 16.4",
      fingerprint: null,
      algorithm: null,
      stages: [
        { kind: "ssh_host_identity", label: "确认 SSH 跳板机身份", status: "ok", message: "主机密钥与已保存指纹一致" },
        { kind: "ssh_authentication", label: "登录 SSH 跳板机", status: "ok", message: "私钥认证通过" },
        { kind: "tunnel", label: "通过 SSH 跳板机打通到数据库的通道", status: "ok", message: "已另开一条到数据库的通道" },
        { kind: "tls", label: "加密连接并核对证书", status: "ok", message: "证书主机名仍是 db.internal" },
        { kind: "database_authentication", label: "登录数据库", status: "ok", message: "已登录 reader" },
        { kind: "server_version", label: "读取数据库版本", status: "ok", message: "PostgreSQL 16.4" },
      ],
    };
  }
  function queryResult(args) {
    const request = args.request ?? {};
    const sqlite = request.engine === "sqlite" || request.connectionId === "sqlite-notes";
    if (request.engine === "sqlserver" && /SET\s+SHOWPLAN_ALL\s+ON/i.test(request.sql ?? "")) {
      // SHOWPLAN_ALL 行集：按 NodeId/Parent 的层次（合成形状与真实输出同列名）。
      const showplanColumns = ["StmtId", "NodeId", "Parent", "PhysicalOp", "LogicalOp", "Argument", "EstimateRows"];
      const showplanRows = [
        ["1", "1", "0", "", "", "SELECT", "1"],
        ["1", "2", "1", "Clustered Index Scan", "Clustered Index Scan", "OBJECT:([dbo].[orders])", "3"],
        ["1", "3", "1", "Sort", "Sort", "ORDER BY:", "3"],
      ];
      return {
        id: "run-capture",
        connectionId: request.connectionId ?? "mssql-report",
        connectionName: request.connectionName ?? "报表 · SQL Server",
        database: request.database ?? "rillsql",
        engine: "sqlserver",
        status: "ok",
        cancel: "idle",
        cancelMessage: "",
        viaSsh: false,
        freshChannel: false,
        startedAt: "2026-10-31T02:10:00.000Z",
        elapsedMs: 12,
        statements: [{ sql: request.sql ?? "", status: "ok", message: "", columns: showplanColumns, rows: showplanRows, truncated: false, affected: null }],
      };
    }
    const columns = sqlite ? ["id", "title", "body"] : ["id", "customer", "status", "total"];
    const rows = sqlite ? noteRows : orderRows.slice(0, 2).map((row) => row.slice(0, 4));
    return {
      id: "run-capture",
      connectionId: request.connectionId ?? "pg-dev",
      connectionName: request.connectionName ?? "书店 · 开发",
      database: request.database ?? "bookstore",
      engine: request.engine ?? "postgres",
      status: "ok",
      cancel: "idle",
      cancelMessage: "",
      viaSsh: !sqlite,
      freshChannel: false,
      startedAt: "2026-10-14T02:10:00.000Z",
      elapsedMs: 18,
      statements: [{ sql: request.sql ?? "", status: "ok", message: "", columns, rows, truncated: false, affected: null }],
    };
  }
  const structure = {
    engine: "postgres",
    database: "bookstore",
    schema: "public",
    name: "orders",
    kind: "table",
    storage: null,
    sqliteVersion: "",
    foreignKeys: null,
    recordedForeignKeys: null,
    rowCount: 128,
    columns: [
      { name: "id", typeName: "integer", nullable: false, defaultSql: "", primary: true, auto: true, generated: false },
      { name: "customer", typeName: "text", nullable: true, defaultSql: "", primary: false, auto: false, generated: false },
      { name: "status", typeName: "text", nullable: false, defaultSql: "'pending'", primary: false, auto: false, generated: false },
      { name: "total", typeName: "numeric", nullable: false, defaultSql: "0", primary: false, auto: false, generated: false },
      { name: "note", typeName: "text", nullable: true, defaultSql: "", primary: false, auto: false, generated: false },
      { name: "created_at", typeName: "timestamp", nullable: false, defaultSql: "now()", primary: false, auto: false, generated: false },
    ],
    indexes: [
      { name: "orders_pkey", unique: true, columns: [{ name: "id", descending: false }], protected: true, reason: "主键" },
      { name: "orders_status_idx", unique: false, columns: [{ name: "status", descending: false }], protected: false, reason: "" },
    ],
    foreignKeyList: [{ name: "orders_customer_fkey", columns: ["customer"], refSchema: "public", refTable: "customers", refColumns: ["name"], onDelete: "NO ACTION", onUpdate: "NO ACTION" }],
    incoming: [],
    displaySql: "create table public.orders (\n  id integer primary key,\n  customer text,\n  status text not null default 'pending',\n  total numeric not null default 0,\n  note text,\n  created_at timestamp not null default now()\n);",
  };
  const callbacks = new Map();
  let callbackId = 0;
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener(_event, id) { callbacks.delete(id); } };
  Object.assign(window, {
    isTauri: true,
    __uiCalls: [],
    __TAURI_INTERNALS__: {
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
      transformCallback(callback) {
        callbackId += 1;
        callbacks.set(callbackId, callback);
        return callbackId;
      },
      unregisterCallback(id) { callbacks.delete(id); },
      runCallback(id, data) { callbacks.get(id)?.(data); },
      callbacks,
      convertFileSrc(filePath) { return filePath; },
      async invoke(command, args = {}) {
        window.__uiCalls.push({ command, args });
        if (command === "plugin:updater|check") return null;
        if (command === "plugin:window|is_maximized") return false;
        if (command === "plugin:window|is_focused") return true;
        if (command.startsWith("plugin:event|")) return 1;
        if (command === "plugin:dialog|open" || command === "plugin:dialog|save") return null;
        if (command === "app_info") {
          return { title: "RillSQL", name_en: "RillSQL", license: "GPL-3.0-only", data_dir: "C:\\Users\\local\\AppData\\Roaming\\RillSQL" };
        }
        if (command === "catalog_sessions") return catalog.map((item) => ({ id: item.id, status: "connected", reason: null }));
        if (command === "catalog_refresh") return catalog;
        if (command === "catalog_page") {
          const schema = catalog.flatMap((item) => item.databases).flatMap((item) => item.schemas).find((item) => item.name === (args.schema ?? ""));
          return { objects: schema?.objects ?? [], total: schema?.total ?? 0, error: null };
        }
        if (command === "env_tags_list") {
          return [
            { id: "dev", label: "开发", tone: "dev", position: 0 },
            { id: "test", label: "测试", tone: "test", position: 1 },
            { id: "prod", label: "生产", tone: "prod", position: 2 },
            { id: "local", label: "本地", tone: "local", position: 3 },
          ];
        }
        if (command === "env_tags_save" || command === "env_tags_delete") return command === "env_tags_save" ? { id: args.id || "tag-new", label: args.label, tone: args.tone, position: 4 } : null;
        if (command === "connections_list") return saved;
        if (command === "connections_save") {
          const draft = args.draft ?? {};
          const id = draft.id || "saved-capture";
          const next = {
            id,
            name: draft.name || draft.host || "未命名",
            engine: draft.engine,
            env: draft.env,
            host: draft.host || null,
            port: draft.port ? Number(draft.port) : null,
            database: draft.database || null,
            username: draft.username || null,
            filePath: draft.filePath || null,
            replicaSet: draft.engine === "mongodb" ? draft.replicaSet || null : null,
            tls: null,
            ssh: draft.sshOn ? { host: draft.sshHost, port: Number(draft.sshPort || 22), username: draft.sshUser, auth: draft.sshAuth, keyPath: draft.sshKey } : null,
            passwordSaved: Boolean(draft.savePassword),
            sshPasswordSaved: false,
            sshHostKey: null,
          };
          const index = saved.findIndex((item) => item.id === id);
          if (index >= 0) saved[index] = { ...saved[index], ...next, sshHostKey: saved[index].sshHostKey };
          else saved.push(next);
          return { connection: saved.find((item) => item.id === id), note: null };
        }
        if (command === "connections_test" || command === "connections_open") {
          const connection = saved.find((item) => item.id === args.id) ?? saved[0];
          const report = testReport(connection);
          if (command === "connections_test") return report;
          return { report, connected: true, foreignKeys: connection.engine === "sqlite" ? true : null };
        }
        if (command === "connections_disconnect" || command === "connections_confirm_host_key" || command === "connections_forget_host_key" || command === "connections_delete") return command === "connections_delete" ? { connection: saved[0], databaseFileRemoved: false } : null;
        if (command === "grid_describe") {
          const schema = schemaFor(args.request?.table, args.request?.engine);
          return { engine: schema.engine, schema: schema.schema, name: schema.name, kind: schema.kind, columns: schema.columns, key: schema.key };
        }
        if (command === "grid_page") {
          const schema = schemaFor(args.request?.table, args.request?.engine);
          return { rows: schema.rows, truncated: false, sql: schema.sql };
        }
        if (command === "grid_count") return { matched: schemaFor(args.request?.table, args.request?.engine).rows.length };
        if (command === "grid_export") return { status: "ok", success: true, writtenRows: 3, path: args.request?.path ?? "orders.csv", residualPath: null, message: "已写入", partial: false };
        if (command === "grid_export_cancel" || command === "grid_commit") return command === "grid_commit" ? { applied: [], failed: null, pendingIds: [], rolledBack: false, rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不会自动撤销。" } : null;
        if (command === "ddl_describe") return { ...structure, name: args.request?.table || structure.name };
        if (command === "ddl_preview") return { blocked: [], risks: [], steps: [{ sql: "select 1;", label: "预览" }], rule: "逐条执行", rebuild: false, target: "orders" };
        if (command === "ddl_execute") return { blocked: [], rule: "逐条执行", target: "orders", rebuild: false, steps: [] };
        // ---- Redis 合成用例（T11）：SCAN 分页/类型化值/危险确认语义，不依赖 docker ----
        if (command === "redis_scan_keys") {
          const cursor = args.cursor ?? 0;
          if (cursor === 0) {
            return {
              cursor: 37,
              done: false,
              keys: [
                { name: "rillsql:hash:order:1001", keyType: "hash", ttlMs: -1 },
                { name: "rillsql:string:welcome", keyType: "string", ttlMs: -1 },
                { name: "rillsql:string:temp-token", keyType: "string", ttlMs: 3_600_000 },
              ],
            };
          }
          return {
            cursor: 0,
            done: true,
            keys: [
              { name: "rillsql:zset:rank", keyType: "zset", ttlMs: 1_800_000 },
              { name: "rillsql:stream:events", keyType: "stream", ttlMs: -1 },
            ],
          };
        }
        if (command === "redis_read_key") {
          const key = args.key ?? "";
          if (key === "rillsql:string:welcome") return { keyType: "string", ttlMs: -1, value: { kind: "string", text: "澄库 Redis 夹具 · 中文值" } };
          if (key === "rillsql:hash:order:1001") {
            return {
              keyType: "hash",
              ttlMs: -1,
              value: { kind: "hash", entries: [{ field: "status", value: "paid" }, { field: "customer", value: "林夏" }, { field: "total", value: "128.5" }] },
            };
          }
          if (key === "rillsql:zset:rank") {
            return {
              keyType: "zset",
              ttlMs: 1_800_000,
              value: { kind: "zset", entries: [{ member: "张伟", score: "90" }, { member: "林夏", score: "85" }] },
            };
          }
          if (key === "rillsql:stream:events") {
            return {
              keyType: "stream",
              ttlMs: -1,
              value: { kind: "stream", messages: [{ id: "1698751000000-0", fields: [{ field: "event", value: "order.created" }, { field: "operator", value: "管理员" }] }] },
            };
          }
          return { keyType: "string", ttlMs: -1, value: { kind: "string", text: "" } };
        }
        if (command === "redis_set_ttl") return { applied: 1 };
        // ---- MongoDB 合成用例（T12）：过滤定位错误/文档 CRUD 断言语义，不依赖 docker ----
        if (command === "mongo_list_databases") {
          return [{ name: "rillsql", collections: 2 }];
        }
        if (command === "mongo_list_collections") {
          return [
            { name: "customers", estimatedDocuments: 3 },
            { name: "orders", estimatedDocuments: 25 },
          ];
        }
        if (command === "mongo_find_documents") {
          const filterJson = (args.filterJson ?? "").trim();
          if (filterJson) {
            // 与后端一致：语法错误给出行列定位，非法扩展 JSON 给出原因。
            try {
              JSON.parse(filterJson);
            } catch (error) {
              throw `过滤语法错误：${error.message} at line 1 column 13`;
            }
            if (filterJson.includes("$numberLong:5")) {
              throw "过滤里的扩展 JSON 值不合法：$numberLong must be a string";
            }
          }
          const page = [
            { idJson: '{"$oid":"65f2c1e4a1b2c3d4e5f61001"}', json: '{"_id":{"$oid":"65f2c1e4a1b2c3d4e5f61001"},"单号":"SO-1001","客户":"林夏","status":"active","总额":17.5,"备注":null}' },
            { idJson: '{"$oid":"65f2c1e4a1b2c3d4e5f61002"}', json: '{"_id":{"$oid":"65f2c1e4a1b2c3d4e5f61002"},"单号":"SO-1002","客户":"周宁","status":"pending","总额":35}' },
          ];
          if (filterJson.includes('"status"') && filterJson.includes("active")) {
            return { total: 1, documents: page.slice(0, 1) };
          }
          return { total: 25, documents: page };
        }
        if (command === "mongo_read_document") {
          return { json: '{"_id":{"$oid":"65f2c1e4a1b2c3d4e5f61001"},"单号":"SO-1001","客户":"林夏","status":"active","总额":17.5}' };
        }
        if (command === "mongo_replace_document") {
          JSON.parse(args.json ?? "{}");
          return { matched: 1, modified: 1 };
        }
        if (command === "mongo_insert_document") {
          JSON.parse(args.json ?? "{}");
          return { insertedIdJson: '{"$oid":"65f2c1e4a1b2c3d4e5f6f001"}' };
        }
        if (command === "mongo_delete_document") {
          if (!args.confirmed) throw "删除文档属于写删操作，需要先在界面确认。";
          return { deleted: 1 };
        }
        if (command === "redis_delete_keys") {
          if (!args.confirmed) throw "删除 1 个键属于写删操作，需要先在界面确认。";
          return { deleted: 1 };
        }
        if (command === "redis_rename_key") {
          if (!args.confirmed) throw "重命名键属于写删操作，需要先在界面确认。";
          return { renamed: true };
        }
        if (command === "redis_exec") {
          const word = (args.command ?? "").trim().toUpperCase();
          if (word === "KEYS") throw "已禁用 KEYS：全量扫描会阻塞 Redis，键空间浏览一律走 SCAN 分页。";
          const dangerous = ["DEL", "UNLINK", "FLUSHDB", "FLUSHALL", "RENAME", "EXPIRE", "PERSIST", "SET", "HSET", "LPOP", "RPOP", "SPOP", "ZREM"];
          if (dangerous.includes(word) && !args.confirmed) throw `${word} 属于写删类命令，需要先在界面确认。`;
          if (word === "GET") return { command: args.command, reply: { kind: "bulk", text: "澄库 Redis 夹具 · 中文值", int: null, items: null } };
          if (word === "DBSIZE") return { command: args.command, reply: { kind: "int", text: null, int: 5, items: null } };
          return { command: args.command, reply: { kind: "status", text: "OK", int: null, items: null } };
        }
        if (command === "query_release") return true;
        if (command === "query_start" || command === "query_poll" || command === "query_cancel") return queryResult(args);
        if (command === "query_history") return history;
        if (command === "query_history_clear") return null;
        throw `未模拟的命令 ${command}`;
      },
    },
  });
}
