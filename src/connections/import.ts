import type { SaveOutcome } from "./api";
import { blankDraft, engineMeta, type ConnectionDraft, type Engine, type SavedConnection, type SecretInput } from "./model";

export interface ImportNote {
  name: string;
  message: string;
}

export interface ImportResult {
  recognized: boolean;
  saved: SavedConnection[];
  skipped: ImportNote[];
  failed: ImportNote[];
  message: string;
}

const EMPTY_SECRETS: SecretInput = { password: "", sshPassword: "", passphrase: "" };
const SECRET_KEY = /pass(?:word|phrase)/i;

type EngineHit = { kind: "supported"; engine: Engine } | { kind: "other"; token: string } | { kind: "none" };

interface PreparedRow {
  name: string;
  draft: ConnectionDraft;
}

/** 丢掉密码和口令字段，不把值抄进连接草稿。 */
function discardSecrets(value: unknown): void {
  if (!value || typeof value !== "object") return;
  if (Array.isArray(value)) {
    for (const item of value) discardSecrets(item);
    return;
  }
  const record = value as Record<string, unknown>;
  for (const key of Object.keys(record)) {
    if (SECRET_KEY.test(key)) delete record[key];
    else discardSecrets(record[key]);
  }
}

function discardSecretNodes(element: Element): void {
  for (const attribute of [...element.attributes]) {
    if (SECRET_KEY.test(attribute.name)) element.removeAttribute(attribute.name);
  }
  for (const child of [...element.children]) {
    if (SECRET_KEY.test(child.localName)) child.remove();
    else discardSecretNodes(child);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function field(source: Record<string, unknown>, key: string): string {
  const value = source[key];
  if (typeof value === "string") return value.trim();
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  return "";
}

function classify(raw: string): EngineHit {
  const trimmed = raw.trim();
  if (!trimmed) return { kind: "none" };
  const jdbc = /jdbc:([A-Za-z0-9]+):/.exec(trimmed);
  if (jdbc) {
    const scheme = jdbc[1];
    switch (scheme.toLowerCase()) {
      case "postgresql":
        return { kind: "supported", engine: "postgres" };
      case "mysql":
        return { kind: "supported", engine: "mysql" };
      case "mariadb":
        return { kind: "supported", engine: "mariadb" };
      case "sqlite":
        return { kind: "supported", engine: "sqlite" };
      default:
        return { kind: "other", token: scheme };
    }
  }
  switch (trimmed.toLowerCase()) {
    case "postgresql":
      return { kind: "supported", engine: "postgres" };
    case "mysql":
      return { kind: "supported", engine: "mysql" };
    case "mariadb":
      return { kind: "supported", engine: "mariadb" };
    case "sqlite":
      return { kind: "supported", engine: "sqlite" };
    case "generic":
    case "custom":
    case "unknown":
      return { kind: "none" };
    default: {
      const lower = trimmed.toLowerCase();
      // mysql8、postgres-jdbc 这类驱动编号没有单独标明引擎，继续看下一项。
      if (lower.includes("jdbc") || /\d/.test(lower)) return { kind: "none" };
      return { kind: "other", token: trimmed };
    }
  }
}

function resolveEngine(provider: string, driver: string, url: string): EngineHit {
  for (const value of [provider, driver, url]) {
    const hit = classify(value);
    if (hit.kind !== "none") return hit;
  }
  return { kind: "none" };
}

function navicatEngine(connType: string): EngineHit {
  switch (connType.trim().toUpperCase()) {
    case "MYSQL":
      return { kind: "supported", engine: "mysql" };
    case "MARIADB":
      return { kind: "supported", engine: "mariadb" };
    case "POSTGRESQL":
    case "POSTGRES":
      return { kind: "supported", engine: "postgres" };
    case "SQLITE":
      return { kind: "supported", engine: "sqlite" };
    default:
      return connType.trim() ? { kind: "other", token: connType.trim() } : { kind: "none" };
  }
}

function enginePhrase(token: string): string {
  const raw = token.trim();
  const key = raw.toLowerCase();
  if (key === "sqlserver" || key === "mssql" || key === "sql server" || key === "microsoft sql server") {
    return key === "sql server" ? "SQL Server" : `SQL Server（${raw}）`;
  }
  return raw;
}

function rowLabel(name: string, fallback: string): string {
  return name.trim() || fallback.trim() || "未命名连接";
}

function skip(name: string, message: string): ImportNote {
  return { name, message: `已跳过「${name}」：${message}` };
}

function sqlitePath(url: string): string {
  const marker = "jdbc:sqlite:";
  const index = url.toLowerCase().indexOf(marker);
  if (index < 0) return "";
  let path = url.slice(index + marker.length).trim();
  const query = path.indexOf("?");
  if (query >= 0) path = path.slice(0, query);
  if (path.startsWith("//")) path = path.slice(2);
  if (/^\/[A-Za-z]:/.test(path)) path = path.slice(1);
  try {
    return decodeURIComponent(path).trim();
  } catch {
    return path.trim();
  }
}

function draftFor(engine: Engine, name: string, host: string, port: string, user: string, database: string, filePath: string): ConnectionDraft {
  const draft = blankDraft(engine);
  draft.name = name;
  draft.tlsMode = "off";
  draft.sshOn = false;
  draft.savePassword = false;
  draft.sshHost = "";
  draft.sshUser = "";
  draft.sshKey = "";
  draft.caFile = "";
  draft.clientCert = "";
  draft.clientKey = "";
  if (engine === "sqlite") {
    draft.filePath = filePath;
    draft.host = "";
    draft.port = "";
    draft.username = "";
    draft.database = "";
    return draft;
  }
  draft.host = host;
  draft.port = port.trim() || engineMeta(engine).port;
  draft.username = user;
  draft.database = database;
  draft.filePath = "";
  return draft;
}

function prepare(name: string, hit: EngineHit, host: string, port: string, user: string, database: string, filePath: string): { row?: PreparedRow; note?: ImportNote } {
  const label = rowLabel(name, hit.kind === "supported" && hit.engine === "sqlite" ? filePath : host);
  if (hit.kind === "other") return { note: skip(label, `${enginePhrase(hit.token)}。`) };
  if (hit.kind === "none") return { note: skip(label, "没有标明引擎。") };
  if (hit.engine !== "sqlite" && !host.trim()) return { note: skip(label, "服务器连接缺少主机。") };
  return { row: { name: label, draft: draftFor(hit.engine, name.trim(), host, port, user, database, filePath) } };
}

function parseDbeaver(parsed: Record<string, unknown>): { rows: PreparedRow[]; skipped: ImportNote[] } | null {
  if (!isRecord(parsed.connections)) return null;
  discardSecrets(parsed);
  const rows: PreparedRow[] = [];
  const skipped: ImportNote[] = [];
  for (const [key, value] of Object.entries(parsed.connections)) {
    const entry = isRecord(value) ? value : null;
    if (!entry) {
      skipped.push(skip(rowLabel("", key), "这条记录不是连接。"));
      continue;
    }
    const configuration = isRecord(entry.configuration) ? entry.configuration : {};
    const provider = field(entry, "provider") || field(configuration, "provider");
    const driver = field(entry, "driver") || field(configuration, "driver");
    const url = field(configuration, "url") || field(entry, "url");
    const name = field(entry, "name") || field(configuration, "name") || key;
    const hit = resolveEngine(provider, driver, url);
    const host = field(configuration, "host") || field(entry, "host");
    const port = field(configuration, "port") || field(entry, "port");
    const user = field(configuration, "user") || field(entry, "user");
    const database = field(configuration, "database") || field(entry, "database");
    const filePath = hit.kind === "supported" && hit.engine === "sqlite" ? sqlitePath(url) : "";
    const prepared = prepare(name, hit, host, port, user, database, filePath);
    if (prepared.note) skipped.push(prepared.note);
    if (prepared.row) rows.push(prepared.row);
  }
  return { rows, skipped };
}

function xmlValue(element: Element, name: string): string {
  const wanted = name.toLowerCase();
  for (const attribute of element.attributes) {
    if (attribute.name.toLowerCase() === wanted) return attribute.value.trim();
  }
  for (const child of element.children) {
    if (child.localName.toLowerCase() === wanted) return (child.textContent ?? "").trim();
  }
  return "";
}

function sameTag(element: Element, name: string): boolean {
  return element.localName.toLowerCase() === name.toLowerCase();
}

function parseNavicat(text: string): { rows: PreparedRow[]; skipped: ImportNote[] } | null {
  const document = new DOMParser().parseFromString(text, "application/xml");
  const root = document.documentElement;
  if (!root || !sameTag(root, "Connections") || document.getElementsByTagName("parsererror").length > 0) return null;
  discardSecretNodes(root);
  const rows: PreparedRow[] = [];
  const skipped: ImportNote[] = [];
  for (const child of root.children) {
    if (!sameTag(child, "Connection")) continue;
    const name = xmlValue(child, "ConnectionName");
    const hit = navicatEngine(xmlValue(child, "ConnType"));
    const filePath = hit.kind === "supported" && hit.engine === "sqlite" ? xmlValue(child, "DatabaseFileName") : "";
    const prepared = prepare(name, hit, xmlValue(child, "Host"), xmlValue(child, "Port"), xmlValue(child, "UserName"), xmlValue(child, "Database"), filePath);
    if (prepared.note) skipped.push(prepared.note);
    if (prepared.row) rows.push(prepared.row);
  }
  return { rows, skipped };
}

function parseImport(text: string): { recognized: boolean; rows: PreparedRow[]; skipped: ImportNote[] } {
  const source = text.replace(/^\uFEFF/, "").trim();
  if (source.startsWith("{")) {
    try {
      const parsed = JSON.parse(source) as unknown;
      if (isRecord(parsed)) {
        const dbeaver = parseDbeaver(parsed);
        if (dbeaver) return { recognized: true, ...dbeaver };
      }
    } catch {
      return { recognized: false, rows: [], skipped: [] };
    }
    return { recognized: false, rows: [], skipped: [] };
  }
  if (source.startsWith("<")) {
    const navicat = parseNavicat(source);
    if (navicat) return { recognized: true, ...navicat };
  }
  return { recognized: false, rows: [], skipped: [] };
}

function failureText(error: unknown): string {
  if (typeof error === "string" && error.trim()) return error.trim();
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  if (error && typeof error === "object" && "message" in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message.trim()) return message.trim();
  }
  return "没有保存。";
}

function summarize(recognized: boolean, saved: SavedConnection[], skipped: ImportNote[], failed: ImportNote[]): string {
  if (!recognized) return "无法识别这份文件，没有新增连接。";
  if (!saved.length && !skipped.length && !failed.length) return "没有可导入的连接，没有新增连接。";
  const parts = [saved.length ? `已导入 ${saved.length} 个连接。` : "没有新增连接。"];
  for (const note of skipped) parts.push(note.message);
  for (const note of failed) parts.push(note.message);
  return parts.join(" ");
}

/** 识别 DBeaver data-sources.json 或 Navicat ncx，并逐条走现有保存。秘密保持为空。 */
export async function importConnectionText(
  text: string,
  save: (draft: ConnectionDraft, secrets: SecretInput) => Promise<SaveOutcome>,
): Promise<ImportResult> {
  const parsed = parseImport(text);
  const saved: SavedConnection[] = [];
  const failed: ImportNote[] = [];
  if (parsed.recognized && parsed.rows.length) {
    for (const row of parsed.rows) {
      try {
        const outcome = await save(row.draft, { ...EMPTY_SECRETS });
        saved.push(outcome.connection);
      } catch (error) {
        failed.push({ name: row.name, message: `「${row.name}」没有保存：${failureText(error)}` });
      }
    }
  }
  return {
    recognized: parsed.recognized,
    saved,
    skipped: parsed.skipped,
    failed,
    message: summarize(parsed.recognized, saved, parsed.skipped, failed),
  };
}
