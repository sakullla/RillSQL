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
  return name.length === 0 || name === "类型未知";
}

export function valueFamily(typeName: string | undefined): ValueFamily {
  if (isUnknownType(typeName)) return "unknown";
  const name = baseType(typeName ?? "");
  if (name === "bool" || name === "boolean") return "bool";
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
  if (name === "datetime" || name === "timestamptz" || name.startsWith("timestamp")) return "datetime";
  if (name === "time" || name.startsWith("time ")) return "time";
  return null;
}

export function typeCaption(column: Pick<Column, "typeName"> | undefined): string {
  if (!column || isUnknownType(column.typeName)) return "类型未知";
  return column.typeName;
}

/** 类型本身不能写回时给出原因。NULL 不能绕过几何、原文和类型未知。 */
export function rejectWrite(column: Column | undefined, value: Cell | undefined): string | null {
  if (value === undefined) return null;
  const family = valueFamily(column?.typeName);
  if (family === "unknown") return "类型未知，不能编辑";
  if (family === "geometry") return "几何类型不能从格子保存";
  if (family === "raw") return "不能判断写回方式，不能编辑";
  if (value === null) return null;
  if (family === "binary" && parseHex(value) == null) return "这不是十六进制";
  if (family === "bool" && !boolLiteral(value)) return "不能写成布尔字面量";
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
  return name === "numeric" || name === "decimal" || name === "number" || name.startsWith("numeric ") || name.startsWith("decimal ");
}

function isFloatName(name: string): boolean {
  return name === "real" || name === "float" || name === "float4" || name === "float8" || name === "double" || name === "double precision" || name.startsWith("float ") || name.startsWith("double ");
}

function isTextName(name: string): boolean {
  return name === "text" || name === "tinytext" || name === "mediumtext" || name === "longtext" || name === "clob" || name === "nclob" || name === "varchar" || name === "nvarchar" || name === "char" || name === "nchar" || name === "character" || name === "character varying" || name === "varchar2" || name === "nvarchar2" || name === "string" || name === "json" || name === "jsonb" || name === "uuid" || name === "enum" || name === "set" || name === "xml" || name === "citext" || name === "name" || name === "bpchar" || name === "varying" || name.startsWith("character ");
}
