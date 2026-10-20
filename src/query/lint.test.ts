import { describe, expect, it } from "vitest";
import { sqlLintDiagnostics } from "./lint";
import { inspectSyntax } from "./split";

describe("editor sql lint", () => {
  it("marks an unclosed quote from its start to end of line", () => {
    expect(sqlLintDiagnostics("SELECT 'abc")).toEqual([
      { from: 7, to: 11, severity: "error", message: "引号没有闭合。" },
    ]);
    expect(inspectSyntax("SELECT 'abc")).toEqual([{ kind: "quote", start: 7, message: "引号没有闭合。" }]);
    /* 未闭合引号跨行时只画到第一个行尾，不吞掉后面的行。 */
    expect(sqlLintDiagnostics("SELECT 'a\nFROM t")).toEqual([
      { from: 7, to: 9, severity: "error", message: "引号没有闭合。" },
    ]);
    expect(inspectSyntax("SELECT `col", "mysql")).toEqual([{ kind: "quote", start: 7, message: "引号没有闭合。" }]);
  });

  it("marks an unclosed block comment", () => {
    expect(sqlLintDiagnostics("SELECT /* note")).toEqual([
      { from: 7, to: 14, severity: "error", message: "块注释没有闭合。" },
    ]);
    expect(inspectSyntax("SELECT /* note")).toEqual([{ kind: "block-comment", start: 7, message: "块注释没有闭合。" }]);
  });

  it("marks an unclosed dollar-quote, with or without a tag", () => {
    expect(inspectSyntax("SELECT $$body")).toEqual([{ kind: "dollar", start: 7, message: "dollar-quote 没有闭合。" }]);
    expect(inspectSyntax("SELECT $fn$body")).toEqual([{ kind: "dollar", start: 7, message: "dollar-quote 没有闭合。" }]);
    expect(sqlLintDiagnostics("SELECT $fn$body")).toEqual([
      { from: 7, to: 15, severity: "error", message: "dollar-quote 没有闭合。" },
    ]);
  });

  it("marks only the first unclosed paren", () => {
    expect(sqlLintDiagnostics("SELECT (1 + (2")).toEqual([
      { from: 7, to: 14, severity: "error", message: "括号没有闭合。" },
    ]);
    expect(inspectSyntax("((SELECT 1)")).toEqual([{ kind: "paren", start: 0, message: "括号没有闭合。" }]);
    /* 多余的右括号不报，与切分时的钳制一致。 */
    expect(sqlLintDiagnostics("SELECT 1)")).toEqual([]);
  });

  it("reports nothing once everything is closed", () => {
    for (const sql of [
      "SELECT 'it''s fine';",
      'SELECT "col" FROM t;',
      "SELECT $$body$$;",
      "SELECT $fn$body$fn$;",
      "/* note */ SELECT (1 + (2));",
      "SELECT '(' FROM t;",
      "SELECT 1 -- don't\nSELECT 2;",
    ]) {
      expect(sqlLintDiagnostics(sql)).toEqual([]);
      expect(sqlLintDiagnostics(sql, "mysql")).toEqual([]);
    }
  });

  it("only reports the unclosed statement in a multi-statement script", () => {
    const sql = "SELECT 1;\nSELECT 'oops";
    const quoteAt = sql.indexOf("'");
    expect(quoteAt).toBeGreaterThan(-1);
    expect(sqlLintDiagnostics(sql)).toEqual([
      { from: quoteAt, to: sql.length, severity: "error", message: "引号没有闭合。" },
    ]);
    expect(inspectSyntax(`${sql} -- tail`)).toEqual([{ kind: "quote", start: quoteAt, message: "引号没有闭合。" }]);
  });

  it("treats # as a comment only for mysql and mariadb", () => {
    const sql = "SELECT 1 # don't";
    const apostrophe = sql.indexOf("'");
    expect(apostrophe).toBeGreaterThan(-1);
    expect(sqlLintDiagnostics(sql, "mysql")).toEqual([]);
    expect(sqlLintDiagnostics(sql, "mariadb")).toEqual([]);
    expect(sqlLintDiagnostics(sql, "postgres")).toEqual([
      { from: apostrophe, to: sql.length, severity: "error", message: "引号没有闭合。" },
    ]);
    expect(sqlLintDiagnostics(sql, "sqlite")).toEqual([
      { from: apostrophe, to: sql.length, severity: "error", message: "引号没有闭合。" },
    ]);
  });

  it("reports an unclosed paren together with a later unclosed quote, ordered by position", () => {
    expect(inspectSyntax("(SELECT 'x")).toEqual([
      { kind: "paren", start: 0, message: "括号没有闭合。" },
      { kind: "quote", start: 8, message: "引号没有闭合。" },
    ]);
  });
});
