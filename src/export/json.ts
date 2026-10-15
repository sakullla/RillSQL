import type { Cell } from "../grid/types";

/** JSON 数组。空值写成 null，空字符串仍是空字符串。 */
export function renderJson(columns: readonly string[], rows: readonly Record<string, Cell>[]): string {
  const objects = rows.map((row) => {
    const object: Record<string, string | null> = {};
    for (const name of columns) object[name] = row[name] ?? null;
    return object;
  });
  return JSON.stringify(objects);
}

export function jsonLines(columns: readonly string[], rows: readonly Record<string, Cell>[]): string[] {
  return rows.map((row) => {
    const object: Record<string, string | null> = {};
    for (const name of columns) object[name] = row[name] ?? null;
    return JSON.stringify(object);
  });
}
