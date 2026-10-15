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

  it("refuses ANALYZE anywhere after strings, identifiers and comments are removed", () => {
    const reason = "计划不使用 ANALYZE，没有发送这条语句。";
    for (const sql of [
      "EXPLAIN(ANALYZE) SELECT 1",
      "EXPLAIN (VERBOSE, ANALYZE) SELECT 1",
      "EXPLAIN /*c*/ ANALYZE SELECT 1",
    ]) {
      expect(explainSql("postgres", sql)).toEqual({ ok: false, reason });
      expect(explainSql("sqlite", sql)).toEqual({ ok: false, reason });
    }
    expect(explainSql("postgres", "EXPLAIN (VERBOSE) SELECT 1")).toEqual({ ok: true, sql: "EXPLAIN (VERBOSE) SELECT 1" });
    expect(explainSql("sqlite", "SELECT status FROM orders")).toEqual({ ok: true, sql: "EXPLAIN QUERY PLAN SELECT status FROM orders" });
    for (const sql of [
      "SELECT 'ANALYZE' AS label",
      'SELECT "ANALYZE"',
      "SELECT `ANALYZE`",
      "SELECT analyzed_at FROM t",
      "SELECT 状态 FROM t",
      "SELECT 1 /* ANALYZE */",
      "SELECT 1 -- ANALYZE",
      "SELECT $$ANALYZE$$",
    ]) {
      expect(explainSql("postgres", sql)).toEqual({ ok: true, sql: `EXPLAIN ${sql}` });
    }
  });

  it("refuses ANALYSE after strings, identifiers, comments and dollar quotes are removed", () => {
    const reason = "计划不使用 ANALYZE，没有发送这条语句。";
    for (const sql of [
      "EXPLAIN ANALYSE SELECT 1",
      "EXPLAIN(ANALYSE) SELECT 1",
      "EXPLAIN (VERBOSE, ANALYSE) SELECT 1",
      "EXPLAIN /*c*/ ANALYSE SELECT 1",
    ]) {
      expect(explainSql("postgres", sql)).toEqual({ ok: false, reason });
      expect(explainSql("sqlite", sql)).toEqual({ ok: false, reason });
    }
    expect(explainSql("postgres", "EXPLAIN ANALYZE SELECT 1")).toEqual({ ok: false, reason });
    for (const sql of ["SELECT 'ANALYSE'", 'SELECT "ANALYSE"', "SELECT $$ANALYSE$$", "SELECT 1 /* ANALYSE */"]) {
      expect(explainSql("postgres", sql)).toEqual({ ok: true, sql: `EXPLAIN ${sql}` });
    }
  });
});
