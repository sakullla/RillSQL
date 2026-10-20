import { engineMeta, type Engine, type TlsMode } from "./model";

export type ParsedConnectionUrl =
  | { ok: true; engine: Engine; host: string; port: string; database: string; username: string; password: string; tlsMode: TlsMode | null }
  | { ok: false; reason: string };

const SCHEME_PREFIX = /^(postgresql|postgres|mysql|mariadb):\/\//i;

/** 解析 postgres://user:pass@host:5432/db?sslmode=… 形式的连接串，供编辑对话框一键填充。 */
export function parseConnectionUrl(text: string): ParsedConnectionUrl {
  const trimmed = text.trim();
  const matched = SCHEME_PREFIX.exec(trimmed);
  if (!matched) return { ok: false, reason: "只支持 postgres://、postgresql://、mysql:// 或 mariadb:// 开头的连接串。" };
  const scheme = matched[1].toLowerCase();
  const engine: Engine = scheme === "mysql" ? "mysql" : scheme === "mariadb" ? "mariadb" : "postgres";
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return { ok: false, reason: "连接串格式无法解析，请检查主机和端口的写法。" };
  }
  // WHATWG URL 对 IPv6 主机保留方括号，表单里按裸地址填写。
  const host = url.hostname.replace(/^\[(.+)\]$/, "$1");
  if (!host) return { ok: false, reason: "连接串里没有主机地址。" };
  return {
    ok: true,
    engine,
    host,
    port: url.port || engineMeta(engine).port,
    database: url.pathname.replace(/^\//, ""),
    username: decodeLoose(url.username),
    password: decodeLoose(url.password),
    tlsMode: tlsModeFrom(url),
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

/** 仅当连接串写了 sslmode 时给出对应的 TLS 档位；没写返回 null，调用方不覆盖表单。 */
function tlsModeFrom(url: URL): TlsMode | null {
  if (!url.searchParams.has("sslmode")) return null;
  const sslmode = url.searchParams.get("sslmode");
  if (sslmode === "disable") return "off";
  if (sslmode === "require" || sslmode === "prefer" || sslmode === "allow") return "require";
  if (sslmode === "verify-full" || sslmode === "verify-ca") return "verify";
  return null;
}
