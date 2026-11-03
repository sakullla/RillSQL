import { engineMeta, type Engine, type TlsMode } from "./model";
import { tr } from "../i18n";

export type ParsedConnectionUrl =
  | {
      ok: true;
      engine: Engine;
      host: string;
      port: string;
      database: string;
      username: string;
      password: string;
      tlsMode: TlsMode | null;
      /** sqlite:// 连接串给出的是文件路径而不是主机。 */
      filePath?: string;
      /** mongodb:// / mongodb+srv:// 整串保存到主机字段；这里保留解析出的裸地址供提示。 */
      original?: { host: string; port: string };
    }
  | { ok: false; reason: string };

/* 覆盖主流客户端导出的串：postgres/postgresql、mysql、mariadb、sqlserver/mssql、sqlite、
   redis/rediss、mongodb/mongodb+srv；统一允许 jdbc: 前缀（DBeaver / DataGrip / IntelliJ 导出格式）。 */
const SCHEME_PREFIX = /^(?:jdbc:)?(postgresql|postgres|mysql|mariadb|sqlserver|mssql|sqlite|redis|rediss|mongodb|mongodb\+srv):/i;

function engineOf(scheme: string): Engine {
  if (scheme === "mysql") return "mysql";
  if (scheme === "mariadb") return "mariadb";
  if (scheme === "sqlserver" || scheme === "mssql") return "sqlserver";
  if (scheme === "sqlite") return "sqlite";
  if (scheme === "redis" || scheme === "rediss") return "redis";
  if (scheme === "mongodb" || scheme === "mongodb+srv") return "mongodb";
  return "postgres";
}

/** 解析 postgres://user:pass@host:5432/db?sslmode=… 形式的连接串，供编辑对话框一键填充。
 * 兼容面：
 * - jdbc: 前缀（jdbc:postgresql://、jdbc:mysql://、jdbc:mariadb://、jdbc:sqlite:…）原样识别；
 * - SQL Server 的 JDBC 分号属性串（…;databaseName=db;user=u;password=p;encrypt=true）；
 * - SQLite 的 sqlite:///path.db、sqlite:relative.db、jdbc:sqlite:C:/… 给出文件路径；
 * - MySQL 系认 ssl-mode / sslMode / useSSL，MongoDB/Redis 认 tls / ssl 查询参数；
 * - rediss:// 与 mongodb+srv:// 隐含 TLS（无 CA 材料时按 require 档填入）。 */
export function parseConnectionUrl(text: string): ParsedConnectionUrl {
  const trimmed = text.trim();
  const matched = SCHEME_PREFIX.exec(trimmed);
  if (!matched) return { ok: false, reason: tr("connections.url.unsupported") };
  const scheme = matched[1].toLowerCase();
  const engine = engineOf(scheme);
  /* sqlite 不走 URL 解析：sqlite:///abs.db、sqlite://rel.db、sqlite:rel.db、jdbc:sqlite:C:/… 都是文件路径。 */
  if (engine === "sqlite") {
    let path = trimmed.slice(matched[0].length).replace(/^\/\//, "");
    const query = path.indexOf("?");
    if (query >= 0) path = path.slice(0, query);
    try {
      path = decodeURIComponent(path).trim();
    } catch {
      path = path.trim();
    }
    /* sqlite:///C:/… 的三斜杠形态按 Windows 盘符路径处理（与 DBeaver 的导出一致）。 */
    if (/^\/[A-Za-z]:/.test(path)) path = path.slice(1);
    if (!path) return { ok: false, reason: tr("connections.url.noHost") };
    return { ok: true, engine, host: "", port: "", database: "", username: "", password: "", tlsMode: null, filePath: path };
  }
  if (!trimmed.slice(matched[0].length).startsWith("//")) {
    return { ok: false, reason: tr("connections.url.parseFailed") };
  }
  /* 去掉 jdbc: 前缀再交给 URL 解析；SQL Server 的 ;key=value 属性段先转成查询串。 */
  const urlText = `${scheme}:${trimmed.slice(matched[0].length)}`;
  const normalized = engine === "sqlserver" ? normalizeSqlServerUrl(urlText, `${scheme}:`) : urlText;
  let url: URL;
  try {
    url = new URL(normalized);
  } catch {
    return { ok: false, reason: tr("connections.url.parseFailed") };
  }
  // WHATWG URL 对 IPv6 主机保留方括号，表单里按裸地址填写。
  const host = url.hostname.replace(/^\[(.+)\]$/, "$1");
  if (!host) return { ok: false, reason: tr("connections.url.noHost") };
  if (engine === "mongodb") {
    // MongoDB：整串保存进主机字段（后端按连接串解析，串里的参数优先）；
    // SRV 形态端口由 DNS 记录决定，表单端口留空。
    const port = scheme === "mongodb+srv" ? "" : url.port || engineMeta(engine).port;
    return {
      ok: true,
      engine,
      host: trimmed,
      port,
      database: url.pathname.replace(/^\//, ""),
      username: decodeLoose(url.username),
      password: decodeLoose(url.password),
      // mongodb+srv:// 隐含 TLS：没有额外 CA 材料时按“加密但不核对证书”档填入（与后端 Require 档对应）。
      tlsMode: scheme === "mongodb+srv" ? "require" : tlsModeFrom(url, engine),
      original: { host, port },
    };
  }
  const queryUser = engine === "sqlserver" ? queryParam(url, "user") : "";
  const queryPassword = engine === "sqlserver" ? queryParam(url, "password") : "";
  return {
    ok: true,
    engine,
    host,
    port: url.port || engineMeta(engine).port,
    database: databaseOf(url, engine),
    username: decodeLoose(url.username) || queryUser,
    password: decodeLoose(url.password) || queryPassword,
    // rediss:// 表示 TLS：没有额外 CA 材料时按“加密但不核对证书”档填入（与后端 Require 档对应）。
    tlsMode: scheme === "rediss" ? "require" : tlsModeFrom(url, engine),
  };
}

/** 查询参数按键不区分大小写取值（jdbc:sqlserver 用 camelCase）。 */
function queryParam(url: URL, wanted: string): string {
  for (const [key, value] of url.searchParams.entries()) {
    if (key.toLowerCase() === wanted) return value;
  }
  return "";
}

/** jdbc:sqlserver://host:1433;databaseName=db;user=u;… 的 ;key=value 段转成 ?key=value&… 交给 URL。 */
function normalizeSqlServerUrl(trimmed: string, prefix: string): string {
  const rest = trimmed.slice(prefix.length);
  const semi = rest.indexOf(";");
  if (semi < 0) return trimmed;
  return `${prefix}${rest.slice(0, semi)}?${rest.slice(semi + 1).replaceAll(";", "&")}`;
}

/** SQL Server 优先取 databaseName 属性；其他引擎取路径段。 */
function databaseOf(url: URL, engine: Engine): string {
  if (engine === "sqlserver") {
    for (const [key, value] of url.searchParams.entries()) {
      if (key.toLowerCase() === "databasename" && value.trim()) return value.trim();
    }
  }
  return url.pathname.replace(/^\//, "");
}

/** URL 属性保留百分号编码，这里解码成用户原始输入；畸形百分号按原文保留。 */
function decodeLoose(value: string): string {
  if (!value) return "";
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

/** 仅当连接串写了 TLS 相关参数时给出对应档位；没写返回 null，调用方不覆盖表单。
 * 覆盖面：postgres 的 sslmode；MySQL/MariaDB 的 ssl-mode/sslMode/useSSL；SQL Server 的
 * encrypt/trustServerCertificate（jdbc:sqlserver 的 camelCase 按不区分大小写扫描）；
 * MongoDB/Redis 的 tls/ssl 开关。 */
function tlsModeFrom(url: URL, engine: Engine): TlsMode | null {
  // URLSearchParams 的键区分大小写；jdbc:sqlserver 用 camelCase（encrypt / trustServerCertificate），按不区分大小写扫描。
  const param = (wanted: string): string | null => {
    for (const [key, value] of url.searchParams.entries()) {
      if (key.toLowerCase() === wanted) return value.toLowerCase();
    }
    return null;
  };
  const encrypt = param("encrypt");
  if (encrypt !== null) {
    if (encrypt === "false" || encrypt === "no") return "off";
    const trust = param("trustservercertificate") ?? "";
    if (trust === "true" || trust === "yes") return "require";
    if (encrypt === "true" || encrypt === "yes" || encrypt === "strict" || encrypt === "mandatory") return "verify";
  }
  const sslmode = param("sslmode");
  if (sslmode !== null) {
    if (sslmode === "disable") return "off";
    if (sslmode === "require" || sslmode === "prefer" || sslmode === "allow") return "require";
    if (sslmode === "verify-full" || sslmode === "verify-ca") return "verify";
  }
  if (engine === "mysql" || engine === "mariadb") {
    const sslMode = param("ssl-mode") ?? param("sslmode");
    if (sslMode !== null) {
      if (sslMode === "disabled" || sslMode === "disable") return "off";
      if (sslMode === "verify_ca" || sslMode === "verify_identity") return "verify";
      if (sslMode === "required" || sslMode === "preferred") return "require";
    }
    const useSsl = param("usessl");
    if (useSsl !== null) return useSsl === "false" || useSsl === "no" ? "off" : "require";
  }
  if (engine === "mongodb" || engine === "redis") {
    const flag = param("tls") ?? param("ssl");
    if (flag !== null) return flag === "true" || flag === "yes" || flag === "1" ? "require" : "off";
  }
  return null;
}
