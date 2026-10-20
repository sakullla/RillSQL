import { KIND_LABEL, objectLabel } from "../catalog/tree";
import type { CatalogConnection, ObjectKind } from "../catalog/types";
import { engineMeta } from "../connections/model";
import { CONNECTION_STATUS_TEXT } from "../shell/status-text";

export const SQL_KEYWORDS = [
  "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "EXISTS",
  "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "ALTER", "DROP",
  "TABLE", "VIEW", "INDEX", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "ON",
  "GROUP", "BY", "ORDER", "LIMIT", "OFFSET", "AS", "DISTINCT", "HAVING", "UNION",
  "ALL", "CASE", "WHEN", "THEN", "ELSE", "END", "WITH", "BEGIN", "COMMIT", "ROLLBACK",
] as const;

export interface CompletionObject {
  label: string;
  name: string;
  schema: string;
  kind: ObjectKind;
}

export interface MetadataNote {
  keywordsOnly: boolean;
  note: string;
  objects: CompletionObject[];
}

export interface CompletionItem {
  label: string;
  kind: "keyword" | ObjectKind | "column";
  detail: string;
}

export interface CachedColumn {
  label: string;
  detail: string;
}

export function metadataNote(connection: CatalogConnection | null, database: string | null): MetadataNote {
  if (!connection || database == null) {
    return { keywordsOnly: true, note: "未选择执行目标：补全只提供关键字。字段不可用。", objects: [] };
  }
  if (connection.status !== "connected") {
    return { keywordsOnly: true, note: `${connection.name} 当前${CONNECTION_STATUS_TEXT[connection.status]}：补全只提供关键字。字段不可用。`, objects: [] };
  }
  if (connection.error) {
    return { keywordsOnly: true, note: `读取 ${connection.name} 的表和视图失败（${connection.error}）：补全只提供关键字。字段不可用。`, objects: [] };
  }
  const db = connection.databases.find((item) => item.name === database);
  if (!db) {
    return { keywordsOnly: true, note: `还没有读到数据库 ${database || "main"} 的表和视图：补全只提供关键字。字段不可用。`, objects: [] };
  }
  if (db.error) {
    return { keywordsOnly: true, note: `读取数据库的表和视图失败（${db.error}）：补全只提供关键字。字段不可用。`, objects: [] };
  }
  const failed = db.schemas.filter((schema) => schema.error);
  const ready = db.schemas.filter((schema) => !schema.error);
  if (db.schemas.length > 0 && ready.length === 0) {
    const reason = failed.map((schema) => `${schema.name || "默认"}：${schema.error}`).join("；");
    return { keywordsOnly: true, note: `读取表和视图失败（${reason}）：补全只提供关键字。字段不可用。`, objects: [] };
  }
  const objects: CompletionObject[] = [];
  for (const schema of ready) {
    for (const object of schema.objects) {
      if (object.stale) continue;
      const shown = objectLabel(connection.engine, object);
      objects.push({
        label: schema.name ? `${schema.name}.${shown}` : shown,
        name: object.name,
        schema: schema.name,
        kind: object.kind,
      });
    }
  }
  if (failed.length) {
    const reason = failed.map((schema) => `${schema.name || "默认"}（${schema.error}）`).join("、");
    return { keywordsOnly: false, note: `部分对象读取失败，补全可能不全：${reason}。`, objects };
  }
  if (!objects.length) {
    return { keywordsOnly: true, note: "当前目标还没有读到表或视图：补全只提供关键字。字段不可用。", objects: [] };
  }
  return {
    keywordsOnly: false,
    note: `补全来自 ${connection.name} / ${database || "main"} 已读取的表和视图。已读取的字段会列出，未读取的字段不可用。`,
    objects,
  };
}

export type StatementClause = "select" | "from" | "join" | "where" | "group" | "order" | "having" | "set" | "values";

export interface StatementContext {
  clause: StatementClause | null;
  aliases: Map<string, CompletionObject>;
}

/** 光标前当前语句的子句与 FROM/JOIN 表别名；解析不到子句时 clause 为 null，调用方回退原有行为。 */
export function statementContext(statementBefore: string, objects: readonly CompletionObject[]): StatementContext {
  const tokens = statementTokens(statementBefore);
  return { clause: clauseFromTokens(tokens), aliases: aliasesFromTokens(tokens, objects) };
}

interface StatementToken {
  text: string;
  upper: string;
  word: boolean;
}

const CLAUSE_BY_WORD: Record<string, StatementClause> = {
  SELECT: "select",
  FROM: "from",
  JOIN: "join",
  WHERE: "where",
  GROUP: "group",
  ORDER: "order",
  HAVING: "having",
  SET: "set",
  VALUES: "values",
};

const BARE_ALIAS_BLOCKERS = new Set<string>(SQL_KEYWORDS);

const FIELD_FIRST_CLAUSES = new Set<StatementClause | null>(["select", "where", "group", "order", "having"]);

/** 只留单词与点号、逗号；跳过注释、字符串和引号标识符，避免字面量里的关键字干扰判断。 */
function statementTokens(sql: string): StatementToken[] {
  const tokens: StatementToken[] = [];
  let i = 0;
  while (i < sql.length) {
    const ch = sql[i] ?? "";
    if (/\s/.test(ch)) {
      i += 1;
      continue;
    }
    if (sql.startsWith("--", i)) {
      i = eolAt(sql, i);
      continue;
    }
    if (sql.startsWith("/*", i)) {
      const close = sql.indexOf("*/", i + 2);
      i = close < 0 ? sql.length : close + 2;
      continue;
    }
    if (ch === "'" || ch === '"' || ch === "`") {
      i = quotedEndAt(sql, i);
      continue;
    }
    if (/[A-Za-z_]/.test(ch)) {
      let end = i + 1;
      while (end < sql.length && /[A-Za-z0-9_]/.test(sql[end] ?? "")) end += 1;
      const text = sql.slice(i, end);
      tokens.push({ text, upper: text.toUpperCase(), word: true });
      i = end;
      continue;
    }
    if (ch === "." || ch === ",") tokens.push({ text: ch, upper: ch, word: false });
    i += 1;
  }
  return tokens;
}

/** 从尾部向前找最近的子句关键字，就是光标所在的子句。 */
function clauseFromTokens(tokens: readonly StatementToken[]): StatementClause | null {
  for (let index = tokens.length - 1; index >= 0; index -= 1) {
    const token = tokens[index]!;
    if (!token.word) continue;
    const clause = CLAUSE_BY_WORD[token.upper];
    if (clause) return clause;
  }
  return null;
}

function aliasesFromTokens(tokens: readonly StatementToken[], objects: readonly CompletionObject[]): Map<string, CompletionObject> {
  const aliases = new Map<string, CompletionObject>();
  let i = 0;
  while (i < tokens.length) {
    const token = tokens[i]!;
    if (!token.word || (token.upper !== "FROM" && token.upper !== "JOIN")) {
      i += 1;
      continue;
    }
    i += 1;
    for (;;) {
      const reference = readTableReference(tokens, i);
      if (!reference) break;
      i = reference.next;
      if (reference.alias) {
        const object = matchCompletionObject(reference, objects);
        if (object) aliases.set(reference.alias.toLowerCase(), object);
      }
      const separator = tokens[i];
      if (separator && !separator.word && separator.text === ",") {
        i += 1;
        continue;
      }
      break;
    }
  }
  return aliases;
}

interface TableReference {
  schema: string | null;
  name: string;
  alias: string | null;
  next: number;
}

/** FROM/JOIN 后的一个表引用：schema.name 或 name，带可选 AS 别名或裸别名（标识符且非关键字）。 */
function readTableReference(tokens: readonly StatementToken[], start: number): TableReference | null {
  const first = tokens[start];
  if (!first || !first.word) return null;
  let i = start + 1;
  let schema: string | null = null;
  let name = first.text;
  const dot = tokens[i];
  if (dot && !dot.word && dot.text === ".") {
    const second = tokens[i + 1];
    if (second && second.word) {
      schema = first.text;
      name = second.text;
      i += 2;
    }
  }
  let alias: string | null = null;
  const after = tokens[i];
  if (after && after.word) {
    if (after.upper === "AS") {
      const named = tokens[i + 1];
      if (named && named.word) {
        alias = named.text;
        i += 2;
      }
    } else if (!BARE_ALIAS_BLOCKERS.has(after.upper)) {
      alias = after.text;
      i += 1;
    }
  }
  return { schema, name, alias, next: i };
}

function matchCompletionObject(reference: TableReference, objects: readonly CompletionObject[]): CompletionObject | undefined {
  const wanted = reference.schema ? `${reference.schema}.${reference.name}` : reference.name;
  const byLabel = objects.find((object) => object.label.toLowerCase() === wanted.toLowerCase());
  if (byLabel) return byLabel;
  const schema = reference.schema ? reference.schema.toLowerCase() : null;
  const name = reference.name.toLowerCase();
  if (schema != null) {
    return objects.find((object) => object.schema.toLowerCase() === schema && object.name.toLowerCase() === name);
  }
  return objects.find((object) => object.name.toLowerCase() === name);
}

function eolAt(sql: string, index: number): number {
  let i = index;
  while (i < sql.length && sql[i] !== "\n" && sql[i] !== "\r") i += 1;
  return i;
}

function quotedEndAt(sql: string, start: number): number {
  const quote = sql[start] ?? "";
  let i = start + 1;
  while (i < sql.length) {
    if (sql[i] === quote && sql[i + 1] === quote) {
      i += 2;
      continue;
    }
    if (sql[i] === quote) return i + 1;
    i += 1;
  }
  return sql.length;
}

export function completionPlan(input: {
  prefix: string;
  connection: CatalogConnection | null;
  database: string | null;
  columns?: readonly CachedColumn[];
  context?: { clause: StatementClause | null };
}): {
  items: CompletionItem[];
  note: string;
  keywordsOnly: boolean;
} {
  const meta = metadataNote(input.connection, input.database);
  const prefix = input.prefix.toLowerCase();
  const clause = input.context?.clause ?? null;
  const objectItems: CompletionItem[] = [];
  if (!meta.keywordsOnly) {
    for (const object of meta.objects) {
      const matched = !prefix || object.label.toLowerCase().startsWith(prefix) || object.name.toLowerCase().startsWith(prefix);
      if (!matched) continue;
      objectItems.push({ label: object.label, kind: object.kind, detail: KIND_LABEL[object.kind] });
      if (objectItems.length >= 20) break;
    }
  }
  const columnItems: CompletionItem[] = [];
  if (!meta.keywordsOnly && clause !== "from" && clause !== "join") {
    for (const column of input.columns ?? []) {
      if (columnItems.length >= 40) break;
      if (prefix && !column.label.toLowerCase().startsWith(prefix)) continue;
      columnItems.push({ label: column.label, kind: "column", detail: column.detail });
    }
  }
  const keywordItems: CompletionItem[] = [];
  for (const keyword of SQL_KEYWORDS) {
    if (keywordItems.length >= 40) break;
    if (prefix && !keyword.toLowerCase().startsWith(prefix)) continue;
    if (keyword.toLowerCase() === prefix) continue;
    keywordItems.push({ label: keyword, kind: "keyword", detail: "关键字" });
  }
  let ordered: CompletionItem[];
  if (clause === "from" || clause === "join") ordered = [...objectItems, ...keywordItems];
  else if (FIELD_FIRST_CLAUSES.has(clause)) ordered = [...columnItems, ...objectItems, ...keywordItems];
  else ordered = [...objectItems, ...columnItems, ...keywordItems];
  return { items: ordered.slice(0, 40), note: meta.note, keywordsOnly: meta.keywordsOnly };
}

export interface TargetOption {
  key: string;
  connectionId: string;
  name: string;
  database: string;
  engine: CatalogConnection["engine"];
  envLabel: string;
  envTone: CatalogConnection["envTone"];
  status: CatalogConnection["status"];
  label: string;
}

/** Engine name, environment, and connection state. Same host and database stay distinguishable. */
export function targetDetail(option: TargetOption): string {
  const parts = [engineMeta(option.engine).label];
  if (option.envTone !== "none" && option.envLabel) parts.push(option.envLabel);
  if (option.status !== "connected") parts.push(CONNECTION_STATUS_TEXT[option.status]);
  return parts.join(" · ");
}

export function targetKey(connectionId: string, database: string): string {
  return `${connectionId}\u0000${database}`;
}

export function targetOptions(connections: CatalogConnection[]): TargetOption[] {
  const options: TargetOption[] = [];
  for (const connection of connections) {
    const databases = connection.databases.length
      ? connection.databases
      : [{ name: connection.defaultDatabase || (connection.engine === "sqlite" ? "main" : ""), schemas: [], error: connection.error }];
    for (const database of databases) {
      options.push({
        key: targetKey(connection.id, database.name),
        connectionId: connection.id,
        name: connection.name,
        database: database.name,
        engine: connection.engine,
        envLabel: connection.envLabel,
        envTone: connection.envTone,
        status: connection.status,
        label: `${connection.name} / ${database.name || "main"}`,
      });
    }
  }
  return options;
}

export function connectionById(connections: CatalogConnection[], id: string | null): CatalogConnection | null {
  if (!id) return null;
  return connections.find((item) => item.id === id) ?? null;
}
