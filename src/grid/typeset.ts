import type { Cell, Column, Engine } from "./types";

export type ValueFamily = "unknown" | "bool" | "integer" | "decimal" | "float" | "date" | "time" | "datetime" | "binary" | "geometry" | "text" | "raw";

export type TemporalKind = "date" | "time" | "datetime";

export function baseType(typeName: string): string {
  return typeName
    .trim()
    .toLowerCase()
    .replace(/\(.*\)/g, "")
    .replace(/\s+(unsigned|signed|zerofill)\b/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

export function isUnknownType(typeName: string | undefined): boolean {
  const name = typeName?.trim() ?? "";
  return name.length === 0 || name === tr("grid.typeset.unknown");
}

const PREVIEW_MARK = "\u0001";

export function isBinaryEnvelope(value: string): boolean {
  return value.startsWith(`${PREVIEW_MARK}b:`) && /^\d+$/.test(value.slice(3));
}

/** 页查询只在文本超过 1024 字节时包信封，预览最多 1024 个字符，整段也长于 1024 字节。 */
export function isSyntheticTextPreview(value: string): boolean {
  if (!value.startsWith(`${PREVIEW_MARK}t:`)) return false;
  const body = value.slice(3);
  const cut = body.indexOf(":");
  if (cut <= 0 || !/^\d+$/.test(body.slice(0, cut))) return false;
  const declared = Number(body.slice(0, cut));
  if (!Number.isFinite(declared) || declared <= 1024) return false;
  const preview = body.slice(cut + 1);
  if ([...preview].length > 1024) return false;
  return new TextEncoder().encode(value).length > 1024;
}

/** 未展开的页查询预览。短文本碰巧写成协议形状时不是预览；二进制信封只约束二进制列。 */
export function isPreviewEnvelope(value: string, typeName?: string): boolean {
  if (isBinaryEnvelope(value)) return typeName === undefined || valueFamily(typeName) === "binary";
  return isSyntheticTextPreview(value);
}

export function valueFamily(typeName: string | undefined): ValueFamily {
  if (isUnknownType(typeName)) return "unknown";
  const name = baseType(typeName ?? "");
  // bit 是 T-SQL 的布尔类型；MySQL 的 bit 约定同走布尔家族（与 Rust value_family 镜像）。
  if (name === "bool" || name === "boolean" || name === "bit") return "bool";
  if (isBinaryName(name)) return "binary";
  if (isGeometryName(name)) return "geometry";
  const temporal = temporalKind(name);
  if (temporal) return temporal;
  if (isIntegerName(name)) return "integer";
  if (isDecimalName(name)) return "decimal";
  if (isFloatName(name)) return "float";
  if (isTextName(name)) return "text";
  return "raw";
}

export function temporalKind(typeName: string): TemporalKind | null {
  const name = baseType(typeName);
  if (name === "date") return "date";
  if (name === "datetime" || name === "datetime2" || name === "datetimeoffset" || name === "smalldatetime" || name === "timestamptz" || name.startsWith("timestamp")) return "datetime";
  if (name === "time" || name.startsWith("time ")) return "time";
  return null;
}

export function typeCaption(column: Pick<Column, "typeName"> | undefined): string {
  if (!column || isUnknownType(column.typeName)) return tr("grid.typeset.unknown");
  return column.typeName;
}

/** 列头弱化类型尾注（R2）：类型已知时返回类型名，未知时返回空串——避免逐列重复「类型未知」。
   列头的 data-type 与值检视仍用 typeCaption 给出完整说明。 */
export function columnTypeSuffix(column: Pick<Column, "typeName"> | undefined): string {
  if (!column || isUnknownType(column.typeName)) return "";
  return column.typeName;
}

/** 类型本身不能写回时给出原因。NULL 不能绕过几何、原文和类型未知。resolved 表示主键另读已经返回这段原文。 */
export function rejectWrite(column: Column | undefined, value: Cell | undefined, resolved = false): string | null {
  if (value === undefined) return null;
  const family = valueFamily(column?.typeName);
  if (family === "unknown") return tr("grid.typeset.unknownNotEditable");
  if (family === "geometry") return tr("grid.typeset.geometryNotEditable");
  if (family === "raw") return tr("grid.typeset.rawNotEditable");
  if (value === null) return null;
  if (!resolved && isPreviewEnvelope(value, column?.typeName)) return tr("grid.errors.previewWriteback");
  if (family === "binary" && parseHex(value) == null) return tr("grid.typeset.notHex");
  if (family === "bool" && !boolLiteral(value)) return tr("grid.typeset.notBool");
  return null;
}

export function boolLiteral(value: string): "TRUE" | "FALSE" | null {
  switch (value.trim().toLowerCase()) {
    case "t":
    case "true":
    case "1":
    case "yes":
      return "TRUE";
    case "f":
    case "false":
    case "0":
    case "no":
      return "FALSE";
    default:
      return null;
  }
}

/** 接受偶数字节的十六进制。空串是空的二进制。zz 不是。 */
export function parseHex(value: string): string | null {
  let text = value.trim();
  if (/^\\x/i.test(text)) text = text.slice(2);
  else if (/^0x/i.test(text)) text = text.slice(2);
  else if (/^x'/i.test(text) && text.endsWith("'")) text = text.slice(2, -1);
  if (text === "") return "";
  if (!/^[0-9a-fA-F]+$/.test(text) || text.length % 2 !== 0) return null;
  return text.toLowerCase();
}

export function binaryLiteral(engine: Engine, hex: string): string {
  if (engine === "postgres") return `'\\x${hex}'`;
  // T-SQL 的二进制字面量是 0x 前缀（空二进制是 0x）。
  if (engine === "sqlserver") return `0x${hex}`;
  return `X'${hex}'`;
}

export function numericLiteral(value: string): boolean {
  return /^-?\d+(\.\d+)?$/.test(value);
}

function isBinaryName(name: string): boolean {
  return name === "bytea" || name === "blob" || name === "binary" || name === "varbinary" || name === "image" || name === "raw" || name === "byte" || name.endsWith("blob");
}

function isGeometryName(name: string): boolean {
  return name === "geometry" || name === "geography" || name === "point" || name === "line" || name === "lseg" || name === "box" || name === "path" || name === "polygon" || name === "circle" || name === "multipoint" || name === "multilinestring" || name === "multipolygon" || name === "geometrycollection" || name.startsWith("geometry ") || name.startsWith("geography ");
}

function isIntegerName(name: string): boolean {
  return name === "tinyint" || name === "smallint" || name === "mediumint" || name === "bigint" || name === "integer" || name === "int" || name === "int2" || name === "int4" || name === "int8" || name === "serial" || name === "bigserial" || name === "smallserial" || name === "year";
}

function isDecimalName(name: string): boolean {
  return name === "numeric" || name === "decimal" || name === "number" || name === "money" || name === "smallmoney" || name.startsWith("numeric ") || name.startsWith("decimal ");
}

function isFloatName(name: string): boolean {
  return name === "real" || name === "float" || name === "float4" || name === "float8" || name === "double" || name === "double precision" || name.startsWith("float ") || name.startsWith("double ");
}

function isTextName(name: string): boolean {
  return name === "text" || name === "tinytext" || name === "mediumtext" || name === "longtext" || name === "clob" || name === "nclob" || name === "varchar" || name === "nvarchar" || name === "char" || name === "nchar" || name === "character" || name === "character varying" || name === "varchar2" || name === "nvarchar2" || name === "string" || name === "json" || name === "jsonb" || name === "uuid" || name === "uniqueidentifier" || name === "ntext" || name === "enum" || name === "set" || name === "xml" || name === "citext" || name === "name" || name === "bpchar" || name === "varying" || name.startsWith("character ");
}

import { tr } from "../i18n";