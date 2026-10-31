/** 全仓唯一的引擎元数据源：Engine 联合与 ENGINES 表都在这里，其他模块只 re-export。 */
export type Engine = "mysql" | "mariadb" | "postgres" | "sqlite" | "sqlserver" | "redis" | "mongodb";

export interface EngineMeta {
  id: Engine;
  label: string;
  short: string;
  port: string;
  /** SQL 执行、表网格、结构编辑、对比与五格式导出导入。 */
  relational: boolean;
  /** 集合与文档浏览（MongoDB 形态）。 */
  document: boolean;
  /** 键空间浏览（Redis 形态）。 */
  kv: boolean;
}

/** R14：引擎选择器最终恰 7 项（PostgreSQL/MySQL/MariaDB/SQLite + SQL Server/Redis/MongoDB），不再新增。 */
export const ENGINES: EngineMeta[] = [
  { id: "postgres", label: "PostgreSQL", short: "PG", port: "5432", relational: true, document: false, kv: false },
  { id: "mysql", label: "MySQL", short: "MY", port: "3306", relational: true, document: false, kv: false },
  { id: "mariadb", label: "MariaDB", short: "MA", port: "3306", relational: true, document: false, kv: false },
  { id: "sqlserver", label: "SQL Server", short: "MS", port: "1433", relational: true, document: false, kv: false },
  { id: "sqlite", label: "SQLite", short: "SQL", port: "", relational: true, document: false, kv: false },
  { id: "redis", label: "Redis", short: "RD", port: "6379", relational: false, document: false, kv: true },
  { id: "mongodb", label: "MongoDB", short: "MG", port: "27017", relational: false, document: true, kv: false },
];

export function engineMeta(engine: Engine): EngineMeta {
  return ENGINES.find((item) => item.id === engine) ?? ENGINES[0];
}
