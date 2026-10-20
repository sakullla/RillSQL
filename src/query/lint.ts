import type { Diagnostic } from "@codemirror/lint";
import { inspectSyntax } from "./split";

/** 把结构扫描结果变成编辑器诊断：从未闭合起点画到行尾（无换行时到文末）。纯函数，不依赖编辑器。 */
export function sqlLintDiagnostics(sql: string, engine = "postgres"): Diagnostic[] {
  return inspectSyntax(sql, engine).map((finding) => ({
    from: finding.start,
    to: lineEnd(sql, finding.start),
    severity: "error" as const,
    message: finding.message,
  }));
}

function lineEnd(sql: string, index: number): number {
  let i = index;
  while (i < sql.length && sql[i] !== "\n" && sql[i] !== "\r") i += 1;
  return i;
}
