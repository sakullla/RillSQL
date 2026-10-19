import { isNumericType, sqlCell } from "./sql";
import type { Cell, Column, Engine } from "./types";
import { boolLiteral, isPreviewEnvelope, isSyntheticTextPreview, parseHex, temporalKind, typeCaption, valueFamily } from "./typeset";

export const PREVIEW_BYTES = 1024;
export type NullCopy = "NULL" | "empty" | "N";

const encoder = new TextEncoder();
const MARK = "\u0001";

export function byteLength(text: string): number {
  return encoder.encode(text).length;
}

export interface ShownCell {
  kind: "null" | "empty" | "unset" | "text" | "large";
  label: string;
  title: string;
  /** 查看器里可以出现的文本。未打开的二进制不含内容。 */
  body: string;
}

export interface ShowOptions {
  column?: Column;
  previewBytes?: number;
  opened?: boolean;
  /** 这段文本已经是完整原文。形状像预览信封时也不拆开。 */
  resolved?: boolean;
}

export interface FieldEnvelope {
  kind: "text" | "binary";
  bytes: number;
  preview: string;
}

export function parseEnvelope(value: string): FieldEnvelope | null {
  if (value.startsWith(`${MARK}b:`)) {
    const body = value.slice(3);
    if (!/^\d+$/.test(body)) return null;
    const bytes = Number(body);
    return Number.isFinite(bytes) ? { kind: "binary", bytes, preview: "" } : null;
  }
  if (!isSyntheticTextPreview(value)) return null;
  const body = value.slice(3);
  const cut = body.indexOf(":");
  const bytes = Number(body.slice(0, cut));
  return { kind: "text", bytes, preview: body.slice(cut + 1) };
}

export function showCell(value: Cell | undefined, previewBytesOrOptions: number | ShowOptions = PREVIEW_BYTES): ShownCell {
  const options: ShowOptions = typeof previewBytesOrOptions === "number" ? { previewBytes: previewBytesOrOptions } : previewBytesOrOptions;
  const previewBytes = options.previewBytes ?? PREVIEW_BYTES;
  if (value === undefined) return { kind: "unset", label: "未填", title: "尚未填写", body: "" };
  if (value === null) return { kind: "null", label: "NULL", title: "NULL", body: "" };
  if (value === "") return { kind: "empty", label: "空字符串", title: "空字符串（长度 0）", body: "" };
  const family = options.column ? valueFamily(options.column.typeName) : "text";
  if (family === "binary") return showBinary(value, options.opened === true);
  if (!options.resolved) {
    const envelope = parseEnvelope(value);
    if (envelope?.kind === "text") {
      const preview = previewText(envelope.preview, previewBytes);
      if (!options.opened) return largeText(envelope.bytes, preview);
      return { kind: "text", label: preview, title: preview, body: preview };
    }
  }
  const formatted = formatText(value, family);
  const bytes = byteLength(value);
  if (bytes > previewBytes && !options.opened) return largeText(bytes, previewText(formatted, previewBytes));
  return { kind: "text", label: formatted, title: formatted, body: formatted };
}

export function previewText(value: string, previewBytes = PREVIEW_BYTES): string {
  const bytes = new TextEncoder().encode(value);
  if (bytes.length <= previewBytes) return value;
  let end = previewBytes;
  while (end > 0 && (bytes[end]! & 0xc0) === 0x80) end -= 1;
  const text = new TextDecoder().decode(bytes.slice(0, end));
  return text.endsWith("\uFFFD") ? text.slice(0, -1) : text;
}

export function copyPlain(value: Cell | undefined, nullCopy: NullCopy): string {
  if (value === null || value === undefined) {
    if (nullCopy === "empty") return "";
    if (nullCopy === "N") return "\\N";
    return "NULL";
  }
  return value;
}

/** 矩形和单格共用 NULL 记号。二进制复制十六进制，不复制替换字符。二进制信封只属于二进制列。 */
export function copyCellText(value: Cell | undefined, nullCopy: NullCopy, column?: Column, resolved = false): string {
  if (value === null || value === undefined) return copyPlain(value, nullCopy);
  if (column && valueFamily(column.typeName) === "binary") return parseHex(value) ?? "";
  if (resolved) return value;
  const envelope = parseEnvelope(value);
  if (envelope?.kind === "text") return envelope.preview;
  return value;
}

export function needsFullRead(value: Cell | undefined, column?: Column): boolean {
  if (typeof value !== "string") return false;
  if (isPreviewEnvelope(value, column?.typeName)) return true;
  return !!column && valueFamily(column.typeName) === "binary" && parseHex(value) == null;
}

export function copySqlValue(value: Cell | undefined, column: Column | undefined, engine: Engine = "postgres"): string {
  if (value === undefined) return "DEFAULT";
  return sqlCell(column, value, engine);
}

export function isNumericColumn(column: Column | undefined): boolean {
  return column ? isNumericType(column.typeName) : false;
}

export { typeCaption };

function largeText(bytes: number, preview: string): ShownCell {
  const label = `大字段 · ${bytes} 字节 · ${preview}`;
  return { kind: "large", label, title: "大字段，打开单元格查看完整内容", body: preview };
}

function showBinary(value: string, opened: boolean): ShownCell {
  const envelope = parseEnvelope(value);
  const hex = parseHex(value);
  const bytes = envelope?.kind === "binary" ? envelope.bytes : hex != null ? hex.length / 2 : byteLength(value);
  if (!opened || hex == null) {
    return { kind: "large", label: `二进制 · ${bytes} 字节`, title: "二进制，打开后查看十六进制", body: "" };
  }
  return { kind: "text", label: hex, title: hex, body: hex };
}

function formatText(value: string, family: ReturnType<typeof valueFamily>): string {
  if (family === "bool") return boolLiteral(value) ?? value;
  if (family === "float") return `${value} · 近似`;
  if (family === "date" || family === "time" || family === "datetime") return formatTemporal(value, family);
  return value;
}

function formatTemporal(value: string, family: "date" | "time" | "datetime"): string {
  const matched = /^(\d{4}-\d{2}-\d{2})(?:[ T](\d{2}:\d{2}:\d{2}(?:\.\d+)?))?/.exec(value.trim());
  if (!matched) return value;
  if (family === "date") return matched[1]!;
  if (family === "time") {
    if (matched[2]) return matched[2];
    const time = /^(\d{1,3}:\d{2}:\d{2}(?:\.\d+)?)/.exec(value.trim());
    return time?.[1] ?? value;
  }
  return matched[2] ? `${matched[1]} ${matched[2]}` : matched[1]!;
}

export function columnTemporal(typeName: string): ReturnType<typeof temporalKind> {
  return temporalKind(typeName);
}
