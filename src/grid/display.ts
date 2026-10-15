import { isNumericType, sqlCell } from "./sql";
import type { Cell, Column } from "./types";

export const PREVIEW_BYTES = 1024;
export type NullCopy = "NULL" | "empty" | "N";

const encoder = new TextEncoder();

export function byteLength(text: string): number {
  return encoder.encode(text).length;
}

export interface ShownCell {
  kind: "null" | "empty" | "unset" | "text" | "large";
  label: string;
  title: string;
}

export function showCell(value: Cell | undefined, previewBytes = PREVIEW_BYTES): ShownCell {
  if (value === undefined) return { kind: "unset", label: "未填", title: "尚未填写" };
  if (value === null) return { kind: "null", label: "NULL", title: "NULL" };
  if (value === "") return { kind: "empty", label: "空字符串", title: "空字符串（长度 0）" };
  const bytes = byteLength(value);
  if (bytes > previewBytes) {
    return { kind: "large", label: `大字段 · ${bytes} 字节`, title: "大字段，打开单元格查看完整内容" };
  }
  return { kind: "text", label: value, title: value };
}

export function previewText(value: string, previewBytes = PREVIEW_BYTES): string {
  const bytes = new TextEncoder().encode(value);
  if (bytes.length <= previewBytes) return value;
  let end = previewBytes;
  while (end > 0 && (bytes[end]! & 0xc0) === 0x80) end -= 1;
  return new TextDecoder().decode(bytes.slice(0, end));
}

export function copyPlain(value: Cell | undefined, nullCopy: NullCopy): string {
  if (value === null || value === undefined) {
    if (nullCopy === "empty") return "";
    if (nullCopy === "N") return "\\N";
    return "NULL";
  }
  return value;
}

export function copySqlValue(value: Cell | undefined, column: Column | undefined): string {
  if (value === undefined) return "DEFAULT";
  return sqlCell(column, value);
}

export function isNumericColumn(column: Column | undefined): boolean {
  return column ? isNumericType(column.typeName) : false;
}
