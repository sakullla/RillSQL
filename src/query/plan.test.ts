import { describe, expect, it } from "vitest";
import { explainSql, planStatement } from "./plan";

describe("execution plan selection", () => {
  it("uses the statement at the cursor, or the selection, and refuses empty or multiple statements", () => {
    const sql = "SELECT 1;\nSELECT 2;";
    expect(planStatement(sql, { from: 0, to: 0, head: 0 })).toEqual({ ok: true, statement: "SELECT 1;" });
    const second = sql.indexOf("SELECT 2;");
    expect(planStatement(sql, { from: second, to: second, head: second + 2 })).toEqual({ ok: true, statement: "SELECT 2;" });
    expect(planStatement(sql, { from: second, to: sql.length, head: sql.length })).toEqual({ ok: true, statement: "SELECT 2;" });
    expect(planStatement(sql, { from: 0, to: sql.length })).toEqual({ ok: false, reason: "一次只能查看一条语句的执行计划。" });
    expect(planStatement("   ", { from: 0, to: 0 })).toEqual({ ok: false, reason: "没有可查看计划的语句。" });
    expect(planStatement("/* only */", { from: 0, to: 0 })).toEqual({ ok: false, reason: "没有可查看计划的语句。" });
  });

  it("prefixes one statement with EXPLAIN and never adds ANALYZE", () => {
    expect(explainSql("postgres", "SELECT 1;")).toEqual({ ok: true, sql: "EXPLAIN SELECT 1" });
    expect(explainSql("mysql", "select 1;")).toEqual({ ok: true, sql: "EXPLAIN select 1" });
    expect(explainSql("mariadb", "SELECT 1")).toEqual({ ok: true, sql: "EXPLAIN SELECT 1" });
    expect(explainSql("sqlite", "SELECT 1;")).toEqual({ ok: true, sql: "EXPLAIN QUERY PLAN SELECT 1" });
    expect(explainSql("sqlite", "explain select 1")).toEqual({ ok: true, sql: "EXPLAIN QUERY PLAN select 1" });
    for (const engine of ["postgres", "mysql", "mariadb", "sqlite"]) {
      const explained = explainSql(engine, "SELECT status FROM orders");
      expect(explained.ok).toBe(true);
      if (!explained.ok) continue;
      expect(explained.sql.toLowerCase()).not.toContain("analyze");
    }
    expect(explainSql("postgres", "EXPLAIN ANALYZE SELECT 1")).toEqual({ ok: false, reason: "计划不使用 ANALYZE，没有发送这条语句。" });
    expect(explainSql("postgres", "EXPLAIN (ANALYZE) SELECT 1").ok).toBe(false);
  });
});
