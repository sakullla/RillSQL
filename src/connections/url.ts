import { engineMeta, type Engine, type TlsMode } from "./model";

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
      /** mongodb:// / mongodb+srv:// 整串保存到主机字段；这里保留解析出的裸地址供提示。 */
      original?: { host: string; port: string };
    }
  | { ok: false; reason: string };

const SCHEME_PREFIX = /^(postgresql|postgres|mysql|mariadb|sqlserver|mssql|redis|rediss|mongodb|mongodb\+srv):\/\//i;

/** 解析 postgres://user:pass@host:5432/db?sslmode=… 形式的连接串，供编辑对话框一键填充。
 * SQL Server 的 jdbc:sqlserver:// 与 mssql:// 短串都归一到 sqlserver 引擎（微软自家也用 mssql 短名）。
 * Redis 的 redis://（明文）与 rediss://（TLS）也归一：database 段是库索引。
 * MongoDB 的 mongodb://（明文）与 mongodb+srv://（SRV，隐含 TLS）归一：database 段是认证库（authSource）。 */
export function parseConnectionUrl(text: string): ParsedConnectionUrl {
  const trimmed = text.trim();
  const matched = SCHEME_PREFIX.exec(trimmed);
  if (!matched) return { ok: false, reason: "只支持 postgres://、postgresql://、mysql://、mariadb://、sqlserver://、redis://、rediss://、mongodb:// 或 mongodb+srv:// 开头的连接串。" };
  const scheme = matched[1].toLowerCase();
  const engine: Engine =
    scheme === "mysql"
      ? "mysql"
      : scheme === "mariadb"
        ? "mariadb"
        : scheme === "sqlserver" || scheme === "mssql"
          ? "sqlserver"
          : scheme === "redis" || scheme === "rediss"
            ? "redis"
            : scheme === "mongodb" || scheme === "mongodb+srv"
              ? "mongodb"
              : "postgres";
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return { ok: false, reason: "连接串格式无法解析，请检查主机和端口的写法。" };
  }
  // WHATWG URL 对 IPv6 主机保留方括号，表单里按裸地址填写。
  const host = url.hostname.replace(/^\[(.+)\]$/, "$1");
  if (!host) return { ok: false, reason: "连接串里没有主机地址。" };
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
      tlsMode: scheme === "mongodb+srv" ? "require" : tlsModeFrom(url),
      original: { host, port },
    };
  }
  return {
    ok: true,
    engine,
    host,
    port: url.port || engineMeta(engine).port,
    database: url.pathname.replace(/^\//, ""),
    username: decodeLoose(url.username),
    password: decodeLoose(url.password),
    // rediss:// 表示 TLS：没有额外 CA 材料时按“加密但不核对证书”档填入（与后端 Require 档对应）。
    tlsMode: scheme === "rediss" ? "require" : tlsModeFrom(url),
  };
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

/** 仅当连接串写了 sslmode 时给出对应的 TLS 档位；没写返回 null，调用方不覆盖表单。
 * jdbc:sqlserver 的 encrypt=true/false 与 trustServerCertificate=true 也映射（AAD/OAUTH 不解析）。 */
function tlsModeFrom(url: URL): TlsMode | null {
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
  if (sslmode === null) return null;
  if (sslmode === "disable") return "off";
  if (sslmode === "require" || sslmode === "prefer" || sslmode === "allow") return "require";
  if (sslmode === "verify-full" || sslmode === "verify-ca") return "verify";
  return null;
}
