import { tr } from "../i18n";
import { KIND_LABEL, objectLabel } from "../catalog/tree";
import type { CatalogConnection, ObjectKind } from "../catalog/types";
import { engineMeta } from "../connections/model";
import { connectionStatusText } from "../shell/status-text";
import type { Engine } from "../engines";
import { dialectAssets, quoteIdentifier } from "./completion/dialect";
import { functionCatalog, functionDetail } from "./completion/functions";
import { rankBy, matchKind } from "./completion/rank";
import { snippetCandidates } from "./completion/snippets";
import { parseScope } from "./completion/scope";

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
  /** 读取失败类提示（i18n 后不再用文案子串判断提示可见性）。 */
  failure: boolean;
}

export interface CompletionItem {
  label: string;
  kind: "keyword" | ObjectKind | "column" | "function" | "snippet" | "cte" | "database" | "schema" | "condition";
  detail: string;
}

export interface CachedColumn {
  label: string;
  detail: string;
}

export function metadataNote(connection: CatalogConnection | null, database: string | null): MetadataNote {
  if (!connection || database == null) {
    return { keywordsOnly: true, note: tr("query.complete.noTarget"), objects: [], failure: false };
  }
  if (connection.status !== "connected") {
    return { keywordsOnly: true, note: tr("query.complete.statusNote", { conn: connection.name, status: connectionStatusText(connection.status) }), objects: [], failure: false };
  }
  if (connection.error) {
    return { keywordsOnly: true, note: tr("query.complete.readFailed", { conn: connection.name, error: connection.error }), objects: [], failure: true };
  }
  const db = connection.databases.find((item) => item.name === database);
  if (!db) {
    return { keywordsOnly: true, note: tr("query.complete.noObjects", { db: database || "main" }), objects: [], failure: false };
  }
  if (db.error) {
    return { keywordsOnly: true, note: tr("query.complete.dbFailed", { error: db.error }), objects: [], failure: true };
  }
  const failed = db.schemas.filter((schema) => schema.error);
  const ready = db.schemas.filter((schema) => !schema.error);
  if (db.schemas.length > 0 && ready.length === 0) {
    const reason = failed.map((schema) => `${schema.name || tr("query.complete.defaultSchema")}: ${schema.error}`).join(tr("query.complete.schemaListJoin"));
    return { keywordsOnly: true, note: tr("query.complete.schemasFailed", { reason }), objects: [], failure: true };
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
    const reason = failed.map((schema) => `${schema.name || tr("query.complete.defaultSchema")} (${schema.error})`).join(tr("query.complete.schemaListSoft"));
    return { keywordsOnly: false, note: tr("query.complete.partialFailed", { reason }), objects, failure: true };
  }
  if (!objects.length) {
    return { keywordsOnly: true, note: tr("query.complete.noTables"), objects: [], failure: false };
  }
  return {
    keywordsOnly: false,
    note: tr("query.complete.sourceNote", { conn: connection.name, db: database || "main" }),
    objects,
    failure: false,
  };
}

export type StatementClause = "select" | "from" | "join" | "on" | "where" | "group" | "order" | "having" | "set" | "values" | "use";

const FIELD_FIRST_CLAUSES = new Set<StatementClause>(["select", "where", "group", "order", "having"]);

export interface StatementContext {
  clause: StatementClause | null;
  aliases: Map<string, CompletionObject>;
  ctes: string[];
  target: { alias: string; schema: string | null; name: string } | null;
}

/** 光标前当前语句的子句与 FROM/JOIN 表别名；解析不到子句时 clause 为 null，调用方回退原有行为。 */
export function statementContext(statementBefore: string, objects: readonly CompletionObject[]): StatementContext {
  const scope = parseScope(statementBefore);
  const aliases = new Map<string, CompletionObject>();
  for (const table of scope.tables) {
    const match = objects.find((object) => object.name.toLowerCase() === table.name.toLowerCase() && (!table.schema || object.schema.toLowerCase() === table.schema.toLowerCase()));
    if (match) aliases.set(table.alias.toLowerCase(), match);
  }
  return { clause: scope.clause, aliases, ctes: scope.ctes, target: scope.target };
}

export interface CompletionSource {
  alias: string;
  label: string;
  columns: readonly string[];
}

export interface CompletionContext {
  clause: StatementClause | null;
  ctes?: readonly string[];
  sources?: readonly CompletionSource[];
  /** ON 子句里参与该 JOIN 的两侧（别名或表名）。 */
  onTables?: readonly string[];
  /** UPDATE / INSERT 的目标表（名或别名），用于 SET 与 VALUES 的列收敛。 */
  targetTable?: string;
  /** 外键推断出的 ON 条件候选（已格式化为 "a.col = b.col"）。 */
  onConditions?: readonly string[];
}

/** 按候选的标签做一次前缀/子序列筛选并保留顺序语义。 */
function rankItems(items: readonly CompletionItem[], prefix: string, cap = 40): CompletionItem[] {
  const matched = items.filter((item) => matchKind(item.label, prefix) >= 0);
  return rankBy(matched, prefix, (item) => item.label).slice(0, cap);
}

export function completionPlan(input: {
  prefix: string;
  connection: CatalogConnection | null;
  database: string | null;
  columns?: readonly CachedColumn[];
  context?: CompletionContext;
  engine?: Engine;
}): {
  items: CompletionItem[];
  note: string;
  keywordsOnly: boolean;
} {
  const meta = metadataNote(input.connection, input.database);
  const prefix = input.prefix;
  const clause = input.context?.clause ?? null;
  const engine: Engine = input.engine ?? input.connection?.engine ?? "postgres";
  const assets = dialectAssets(engine);

  const objectItems: CompletionItem[] = [];
  if (!meta.keywordsOnly) {
    for (const object of meta.objects) {
      objectItems.push({ label: object.label, kind: object.kind, detail: tr(KIND_LABEL[object.kind]) });
      if (object.name.toLowerCase() !== object.label.toLowerCase()) {
        objectItems.push({ label: object.name, kind: object.kind, detail: tr(KIND_LABEL[object.kind]) });
      }
    }
  }

  const sources = input.context?.sources ?? [];
  const onTables = new Set((input.context?.onTables ?? []).map((item) => item.toLowerCase()));
  const targetTable = input.context?.targetTable?.toLowerCase() ?? null;
  const columnItems: CompletionItem[] = [];
  if (!meta.keywordsOnly && clause !== "from" && clause !== "join") {
    for (const source of sources) {
      const inOn = clause !== "on" || onTables.size === 0 || onTables.has(source.alias.toLowerCase()) || onTables.has(source.label.toLowerCase());
      if (!inOn) continue;
      const inTarget = clause !== "set" && clause !== "values" ? true : !targetTable || targetTable === source.alias.toLowerCase() || targetTable === source.label.toLowerCase() || targetTable === source.label.split(".").pop()?.toLowerCase();
      if (!inTarget) continue;
      for (const column of source.columns) columnItems.push({ label: column, kind: "column", detail: source.label });
    }
    if (!sources.length && clause !== "on") {
      for (const column of input.columns ?? []) columnItems.push({ label: column.label, kind: "column", detail: column.detail });
    }
  }

  const databaseItems: CompletionItem[] = (input.connection?.databases ?? []).map((item) => ({ label: item.name, kind: "database" as const, detail: tr("query.complete.database") }));
  const current = input.connection?.databases.find((item) => item.name === input.database);
  const schemaItems: CompletionItem[] = (current?.schemas ?? [])
    .map((item) => item.name)
    .filter(Boolean)
    .map((name) => ({ label: name, kind: "schema" as const, detail: tr("query.complete.schema") }));
  const cteItems: CompletionItem[] = (input.context?.ctes ?? []).map((name) => ({ label: name, kind: "cte" as const, detail: tr("query.complete.cte") }));
  const functionItems: CompletionItem[] = functionCatalog(engine)
    .filter((name) => matchKind(name, prefix) >= 0)
    .slice(0, 40)
    .map((name) => ({ label: name, kind: "function" as const, detail: functionDetail(engine, name) }));
  const keywordItems: CompletionItem[] = assets.keywords
    .filter((keyword) => matchKind(keyword, prefix) >= 0 && keyword.toUpperCase() !== prefix.toUpperCase())
    .slice(0, 40)
    .map((keyword) => ({ label: keyword.toUpperCase(), kind: "keyword" as const, detail: tr("query.complete.keyword") }));
  const snippetItems: CompletionItem[] = clause === "from" || clause === "join"
    ? []
    : snippetCandidates(prefix).map((item) => ({ label: item.label, kind: "snippet" as const, detail: item.detail }));

  const conditionItems: CompletionItem[] = (input.context?.onConditions ?? []).map((text) => ({ label: text, kind: "condition" as const, detail: tr("query.complete.joinCondition") }));
  let ordered: CompletionItem[];
  if (clause === "use") {
    ordered = rankItems(databaseItems, prefix);
  } else if (clause === "on") {
    ordered = [...rankItems(conditionItems, prefix, 10), ...rankItems(columnItems, prefix)];
  } else if (clause === "set" || clause === "values") {
    ordered = rankItems(columnItems, prefix);
  } else if (clause === "from" || clause === "join") {
    ordered = [
      ...rankItems(objectItems, prefix),
      ...rankItems(cteItems, prefix, 20),
      ...rankItems(keywordItems, prefix),
    ];
  } else if (clause !== null && FIELD_FIRST_CLAUSES.has(clause)) {
    ordered = [
      ...rankItems(columnItems, prefix),
      ...rankItems(objectItems, prefix),
      ...rankItems(databaseItems, prefix, 20),
      ...rankItems(schemaItems, prefix, 20),
      ...rankItems(cteItems, prefix, 20),
      ...rankItems(functionItems, prefix),
      ...rankItems(keywordItems, prefix),
      ...rankItems(snippetItems, prefix, 6),
    ];
  } else {
    ordered = [
      ...rankItems(databaseItems, prefix, 20),
      ...rankItems(schemaItems, prefix, 20),
      ...rankItems(objectItems, prefix),
      ...rankItems(cteItems, prefix, 20),
      ...rankItems(columnItems, prefix),
      ...rankItems(functionItems, prefix),
      ...rankItems(keywordItems, prefix),
      ...rankItems(snippetItems, prefix, 6),
    ];
  }
  /* 跨组排序：匹配等级优先；全大写前缀视为在写关键字（SQL 惯例），关键字组提前；其余保持组内顺序。 */
  const keywordFirst = prefix.length > 0 && prefix === prefix.toUpperCase();
  ordered = ordered
    .map((item, order) => ({
      item,
      score: matchKind(item.label, prefix),
      keyword: keywordFirst && item.kind === "keyword" ? 0 : 1,
      order,
    }))
    .sort((a, b) => a.score - b.score || a.keyword - b.keyword || a.order - b.order)
    .map((entry) => entry.item);
  const seen = new Set<string>();
  const unique = ordered.filter((item) => {
    const key = `${item.kind}:${item.label.toLowerCase()}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
  return { items: unique.slice(0, 120), note: meta.note, keywordsOnly: meta.keywordsOnly };
}

/** 引用名（别名或表名）按引擎规则加引号；补全插入时使用。 */
export function quotedIdentifier(engine: Engine, name: string): string {
  return quoteIdentifier(engine, name);
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
  if (option.status !== "connected") parts.push(connectionStatusText(option.status));
  return parts.join(" · ");
}

export function targetKey(connectionId: string, database: string): string {
  return `${connectionId}\u0000${database}`;
}

export function targetOptions(connections: CatalogConnection[]): TargetOption[] {
  const options: TargetOption[] = [];
  for (const connection of connections) {
    // 能力门控（ADR-2）：kv/document 引擎没有 SQL 执行器，不出现在查询目标里。
    if (!engineMeta(connection.engine).relational) continue;
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
