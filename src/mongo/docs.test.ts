import { describe, expect, it } from "vitest";
import type { MongoDoc } from "./api";
import { DOC_PAGE_SIZE, canPrev, cellText, columnsOf, describeCount, filterApplied, nextSkip, prettyJson, quickValidateJson } from "./docs";

const doc = (idJson: string, json: string): MongoDoc => ({ idJson, json });

const docs: MongoDoc[] = [
  doc('{"$oid":"65f2c1e4a1b2c3d4e5f60718"}', '{"_id":{"$oid":"65f2c1e4a1b2c3d4e5f60718"},"status":"active","总数":3}'),
  doc('{"$oid":"65f2c1e4a1b2c3d4e5f60719"}', '{"_id":{"$oid":"65f2c1e4a1b2c3d4e5f60719"},"customer":"林夏","标签":["加急"]}'),
];

describe("mongo document grid helpers", () => {
  it("derives columns with _id first and unions top-level fields in encounter order", () => {
    expect(columnsOf(docs)).toEqual(["_id", "status", "总数", "customer", "标签"]);
    expect(columnsOf([])).toEqual(["_id"]);
  });

  it("renders scalar cells as text and structured cells as compact json", () => {
    expect(cellText(docs[1]!.json, "customer")).toBe("林夏");
    expect(cellText(docs[1]!.json, "标签")).toBe('["加急"]');
    expect(cellText(docs[0]!.json, "missing")).toBe("");
    expect(cellText(docs[0]!.json, "_id")).toBe('{"$oid":"65f2c1e4a1b2c3d4e5f60718"}');
  });

  it("pre-validates json locally before any ipc: object required, syntax errors reported", () => {
    expect(quickValidateJson("")).toContain("请输入 JSON");
    expect(quickValidateJson('{"a":1')).toContain("JSON 语法错误");
    expect(quickValidateJson("[1,2]")).toContain("JSON 对象");
    expect(quickValidateJson('{"status":"active"}')).toBeNull();
  });

  it("pages by page size with a zero floor and disabled previous on first page", () => {
    expect(DOC_PAGE_SIZE).toBe(20);
    expect(nextSkip(40, DOC_PAGE_SIZE, 1)).toBe(60);
    expect(nextSkip(40, DOC_PAGE_SIZE, -1)).toBe(20);
    expect(nextSkip(10, DOC_PAGE_SIZE, -1)).toBe(0);
    expect(canPrev(0)).toBe(false);
    expect(canPrev(20)).toBe(true);
  });

  it("formats counts and filter state for the toolbar", () => {
    expect(describeCount(null)).toBe("—");
    expect(describeCount(987)).toBe("987");
    expect(describeCount(1500)).toBe("1.5k");
    expect(describeCount(25000)).toBe("25k");
    expect(filterApplied("  ")).toBe(false);
    expect(filterApplied('{"status":"active"}')).toBe(true);
  });

  it("pretty prints documents for viewing and editing", () => {
    expect(prettyJson('{"a":1,"b":{"c":"中文"}}')).toBe('{\n  "a": 1,\n  "b": {\n    "c": "中文"\n  }\n}');
    expect(prettyJson("{broken")).toBe("{broken");
  });
});
