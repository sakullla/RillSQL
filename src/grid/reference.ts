import { qualifiedObjectName, quoteIdent } from "../catalog/object-ref";
import type { StructureForeignKey } from "../structure/types";
import { isPreviewEnvelope } from "./typeset";
import type { Cell, Engine } from "./types";

/** 一次外键值跳转：命中的外键加上该行各外键列的现值。 */
export interface ReferenceJump {
  foreignKey: StructureForeignKey;
  values: string[];
}

/**
 * 右键列命中外键且整条外键的列值都能当条件时给出跳转。
 * NULL、缺失的列值和未展开的大字段预览不能拼进 WHERE，放弃这条外键。
 */
export function referenceJump(foreignKeyList: readonly StructureForeignKey[], column: string, rowCells: Readonly<Record<string, Cell | undefined>>): ReferenceJump | null {
  for (const foreignKey of foreignKeyList) {
    if (!foreignKey.columns.includes(column)) continue;
    const values: string[] = [];
    let usable = true;
    for (const name of foreignKey.columns) {
      const value = rowCells[name];
      if (value === null || value === undefined || isPreviewEnvelope(value)) {
        usable = false;
        break;
      }
      values.push(value);
    }
    if (usable) return { foreignKey, values };
  }
  return null;
}

/** 引用表的只读查询：引用列按位置对应该行外键列的值，值里的单引号成对转义，多列用 AND 连接。 */
export function referenceQuery(engine: Engine, foreignKey: StructureForeignKey, values: readonly string[]): string {
  const conditions = foreignKey.refColumns.map((name, index) => {
    const literal = (values[index] ?? "").split("'").join("''");
    return `${quoteIdent(engine, name)} = '${literal}'`;
  });
  return `SELECT *\nFROM ${qualifiedObjectName(engine, foreignKey.refSchema, foreignKey.refTable)}\nWHERE ${conditions.join(" AND ")};`;
}
