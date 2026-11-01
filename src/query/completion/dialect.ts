import { MSSQL, MySQL, PostgreSQL, SQLite } from "@codemirror/lang-sql";
import type { Engine } from "../../engines";

export interface DialectAssets {
  keywords: string[];
  builtin: string[];
  types: string[];
  quote: string;
}

const CACHE = new Map<Engine, DialectAssets>();

function words(value: string | undefined): string[] {
  if (!value) return [];
  return [...new Set(value.split(/\s+/).filter(Boolean))];
}

/** 方言关键字/内建/类型取自 lang-sql 的方言声明，避免自建全量词表。 */
export function dialectAssets(engine: Engine): DialectAssets {
  const cached = CACHE.get(engine);
  if (cached) return cached;
  const dialect = engine === "mysql" ? MySQL : engine === "mariadb" ? MySQL : engine === "sqlite" ? SQLite : engine === "sqlserver" ? MSSQL : PostgreSQL;
  const assets: DialectAssets = {
    keywords: words(dialect.spec.keywords),
    builtin: words(dialect.spec.builtin),
    types: words(dialect.spec.types),
    quote: engine === "mysql" || engine === "mariadb" ? "\u0060" : '"',
  };
  CACHE.set(engine, assets);
  return assets;
}

/** 需要引号时才加引号，保留原大小写。 */
export function quoteIdentifier(engine: Engine, name: string): string {
  const quote = dialectAssets(engine).quote;
  const plain = /^[A-Za-z_][A-Za-z0-9_$]*$/.test(name);
  if (plain) return name;
  const escaped = name.split(quote).join(quote + quote);
  return `${quote}${escaped}${quote}`;
}
