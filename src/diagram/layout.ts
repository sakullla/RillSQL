import type { DiagramEdge, DiagramTable } from "./types";

export interface Point {
  x: number;
  y: number;
}

/**
 * 按外键依赖分列：被引用的表在左，引用方在右，自引用不计。
 * 循环外键下层数不会无限增长：路径上已出现的表不再深入。
 */
export function layoutColumns(tables: readonly DiagramTable[], edges: readonly DiagramEdge[]): DiagramTable[][] {
  const parents = new Map<string, Set<string>>();
  const known = new Set(tables.map((table) => table.name));
  for (const edge of edges) {
    if (edge.fromTable === edge.toTable || !known.has(edge.fromTable) || !known.has(edge.toTable)) continue;
    if (!parents.has(edge.fromTable)) parents.set(edge.fromTable, new Set());
    parents.get(edge.fromTable)!.add(edge.toTable);
  }
  const depth = (name: string, path: ReadonlySet<string>): number => {
    let deepest = 0;
    for (const parent of parents.get(name) ?? []) {
      if (!path.has(parent)) deepest = Math.max(deepest, 1 + depth(parent, new Set([...path, parent])));
    }
    return deepest;
  };
  const columns: DiagramTable[][] = [];
  for (const table of tables) {
    const level = depth(table.name, new Set([table.name]));
    while (columns.length <= level) columns.push([]);
    columns[level]!.push(table);
  }
  return columns;
}

/** 水平出入的三次贝塞尔；同列的表用向外弯出的半环连接。 */
export function edgePath(start: Point, end: Point): string {
  const sameSide = Math.abs(start.x - end.x) < 1;
  const bend = sameSide ? 56 : Math.max(36, Math.abs(end.x - start.x) / 2);
  const direction = end.x >= start.x ? 1 : -1;
  const startX = sameSide ? start.x + bend : start.x + bend * direction;
  const endX = sameSide ? end.x + bend : end.x - bend * direction;
  return `M ${start.x} ${start.y} C ${startX} ${start.y}, ${endX} ${end.y}, ${end.x} ${end.y}`;
}
