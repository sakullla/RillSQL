import { flushPromises, mount } from "@vue/test-utils";
import { crc32 } from "node:zlib";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as XLSX from "@e965/xlsx";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import ImportDialog from "./ImportDialog.vue";
import { parseImportCsv } from "./import";
import { parseImportXlsx } from "./xlsx";
import type { TableTarget } from "../grid/target";
import type { TableSchema } from "../grid/types";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const target: TableTarget = { connectionId: "shop", database: "app", schema: "public", table: "customers", engine: "postgres" };
const schema: TableSchema = {
  engine: "postgres",
  schema: "public",
  name: "customers",
  kind: "table",
  key: ["id"],
  columns: [
    { name: "id", typeName: "integer", nullable: false, primary: true, unique: true, auto: true },
    { name: "name", typeName: "text", nullable: false, primary: false, unique: false, auto: false },
    { name: "email", typeName: "text", nullable: false, primary: false, unique: true, auto: false },
    { name: "note", typeName: "text", nullable: true, primary: false, unique: false, auto: false },
  ],
};

function dialog() {
  return mount(ImportDialog, { props: { open: true, target, schema, tables: [{ schema: "public", table: "customers" }] } });
}

async function chooseFile(wrapper: ReturnType<typeof dialog>, name: string, contents: string) {
  const input = wrapper.get("input[type='file']");
  const file = new File([contents], name, { type: "text/plain" });
  Object.defineProperty(input.element, "files", { value: { 0: file, length: 1, item: () => file } });
  await input.trigger("change");
  await flushPromises();
}

async function pickDelimiter(wrapper: ReturnType<typeof dialog>, option: string) {
  await wrapper.get('[role="combobox"][aria-label="CSV 分隔符"]').trigger("click");
  const match = wrapper.findAll('[role="option"]').find((node) => node.text() === option);
  expect(match, option).toBeTruthy();
  await match!.trigger("click");
  await flushPromises();
}

describe("import into an existing table", () => {
  beforeEach(() => {
    invoke.mockReset();
    resetCatalog();
  });

  it("writes two mapped csv rows and reports the count", async () => {
    invoke.mockResolvedValue({
      applied: [
        { id: "import-1", sql: "INSERT", label: "新增 1 行" },
        { id: "import-2", sql: "INSERT", label: "新增 1 行" },
      ],
      failed: null,
      pendingIds: [],
      rolledBack: false,
      rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。",
    });
    const wrapper = dialog();
    await chooseFile(wrapper, "rows.csv", "name,email,note\r\n甲,a@example.com,\r\n乙,b@example.com,备注");
    expect(wrapper.text()).toContain("将写入 2 行");
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find((item) => item[0] === "grid_commit");
    const changes = call?.[1].request.changes as { values: { column: string; value: string | null }[] }[];
    expect(changes).toHaveLength(2);
    expect(changes[0]?.values).toEqual(expect.arrayContaining([{ column: "name", value: "甲" }, { column: "email", value: "a@example.com" }]));
    expect(JSON.stringify(changes)).not.toContain("未提交");
    expect(wrapper.get("[data-import-result]").text()).toContain("已写入 2 行");
    wrapper.unmount();
  });

  it("does not write when import is cancelled or a required column is missing", async () => {
    const cancelled = dialog();
    await chooseFile(cancelled, "rows.csv", "name,email\r\n甲,a@example.com");
    await cancelled.get(".btn").trigger("click");
    expect(cancelled.emitted("close")).toHaveLength(1);
    expect(invoke).not.toHaveBeenCalled();
    cancelled.unmount();

    const missing = dialog();
    await chooseFile(missing, "rows.csv", "name\r\n甲");
    expect(missing.text()).toContain("必填字段 email 没有对应列");
    expect(missing.get(".btn.primary").attributes("disabled")).toBeDefined();
    await missing.get(".btn.primary").trigger("click");
    expect(invoke).not.toHaveBeenCalled();
    missing.unmount();
  });

  it("rejects a json value that is not an object array before writing", async () => {
    const wrapper = dialog();
    await wrapper.get("input[value='json']").setValue(true);
    await chooseFile(wrapper, "rows.json", "{\"name\":\"甲\"}");
    expect(wrapper.text()).toContain("JSON 必须是由对象组成的数组");
    expect(invoke).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("shows the failed json row and does not call the import a full success", async () => {
    invoke.mockResolvedValue({
      applied: [{ id: "import-1", sql: "INSERT", label: "新增 1 行" }],
      failed: { id: "import-2", sql: "INSERT", label: "新增 1 行", message: "invalid input syntax for type integer" },
      pendingIds: ["import-2"],
      rolledBack: false,
      rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。",
    });
    const wrapper = dialog();
    await wrapper.get("input[value='json']").setValue(true);
    await chooseFile(wrapper, "rows.json", "[{\"name\":\"甲\",\"email\":\"a@example.com\"},{\"name\":\"乙\",\"email\":\"bad\"}]");
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    const text = wrapper.get("[data-import-result]").text();
    expect(text).toContain("第 2 行失败");
    expect(text).toContain("invalid input syntax for type integer");
    expect(text).toContain("不是全部成功");
    expect(text).not.toContain("已写入 2 行");
    wrapper.unmount();
  });

  it("shows the catalog connection, table and environment before writing, and cancel writes nothing", async () => {
    replaceCatalog([{
      id: "shop",
      name: "Shop",
      engine: "postgres",
      env: "prod",
      envLabel: "生产",
      envTone: "prod",
      status: "connected",
      defaultDatabase: "app",
      foreignKeys: null,
      error: null,
      databases: [],
    }]);
    const wrapper = dialog();
    const targetText = wrapper.get("[data-import-target]").text();
    expect(targetText).toContain("Shop");
    expect(targetText).toContain("customers");
    expect(targetText).toContain("生产");
    await chooseFile(wrapper, "rows.csv", "name,email,note\r\n甲,a@example.com,\r\n乙,b@example.com,备注");
    expect(wrapper.text()).toContain("将写入 2 行");
    expect(wrapper.get("[data-import-target]").text()).toContain("生产");
    expect(invoke).not.toHaveBeenCalled();
    await wrapper.findAll("button").find((button) => button.text() === "取消")!.trigger("click");
    expect(wrapper.emitted("close")).toHaveLength(1);
    expect(invoke).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("reparses the file and rebuilds the mapping when the csv delimiter changes", async () => {
    invoke.mockResolvedValue({
      applied: [{ id: "import-1", sql: "INSERT", label: "新增 1 行" }],
      failed: null,
      pendingIds: [],
      rolledBack: false,
      rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。",
    });
    const wrapper = dialog();
    expect(wrapper.get('[role="combobox"][aria-label="CSV 分隔符"]').text()).toBe("逗号 ,");
    await chooseFile(wrapper, "rows.csv", "name;email\r\n甲;a@example.com");
    expect(wrapper.text()).toContain("必填字段 name、email 没有对应列");
    expect(wrapper.get(".btn.primary").attributes("disabled")).toBeDefined();
    await pickDelimiter(wrapper, "分号 ;");
    expect(wrapper.text()).not.toContain("必填字段");
    expect(wrapper.text()).toContain("将写入 1 行");
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find((item) => item[0] === "grid_commit");
    const changes = call?.[1].request.changes as { values: { column: string; value: string | null }[] }[];
    expect(changes?.[0]?.values).toEqual(expect.arrayContaining([{ column: "name", value: "甲" }, { column: "email", value: "a@example.com" }]));
    wrapper.unmount();
  });

  it("imports tab separated rows after choosing the tab delimiter", async () => {
    const wrapper = dialog();
    await pickDelimiter(wrapper, "制表符");
    await chooseFile(wrapper, "rows.tsv", "name\temail\r\n甲\ta@example.com");
    expect(wrapper.text()).toContain("将写入 1 行");
    expect(wrapper.get('[role="combobox"][aria-label="name 对应的列"]').text()).toBe("name");
    wrapper.unmount();
  });

  it("resets the delimiter when the dialog reopens and accepts .tsv files", async () => {
    const wrapper = dialog();
    expect(wrapper.get("input[type='file']").attributes("accept")).toContain(".tsv");
    await pickDelimiter(wrapper, "分号 ;");
    await wrapper.setProps({ open: false });
    await wrapper.setProps({ open: true });
    expect(wrapper.get('[role="combobox"][aria-label="CSV 分隔符"]').text()).toBe("逗号 ,");
    wrapper.unmount();
  });
});

describe("parseImportCsv delimiter", () => {
  it("parses semicolon separated csv into two columns and two rows", () => {
    const outcome = parseImportCsv("a;b\n1;2", ";");
    expect(outcome).toEqual({ ok: true, data: { columns: ["a", "b"], rows: [{ a: "1", b: "2" }] } });
  });

  it("parses tab separated files into two columns and two rows", () => {
    const outcome = parseImportCsv("a\tb\n1\t2", "\t");
    expect(outcome).toEqual({ ok: true, data: { columns: ["a", "b"], rows: [{ a: "1", b: "2" }] } });
  });

  it("keeps the comma behavior when no delimiter is passed", () => {
    expect(parseImportCsv("a,b\n1,2")).toEqual({ ok: true, data: { columns: ["a", "b"], rows: [{ a: "1", b: "2" }] } });
    expect(parseImportCsv("a;b\n1;2")).toEqual({ ok: true, data: { columns: ["a;b"], rows: [{ "a;b": "1;2" }] } });
  });
});

function xlsxBytes(sheets: { name: string; rows: (string | null)[][] }[]): Uint8Array {
  const book = XLSX.utils.book_new();
  for (const item of sheets) {
    const table: XLSX.WorkSheet = {};
    item.rows.forEach((row, r) => {
      row.forEach((value, c) => {
        if (value === null) return;
        table[XLSX.utils.encode_cell({ r, c })] = { t: "s", v: value };
      });
    });
    const width = Math.max(0, ...item.rows.map((row) => row.length));
    if (item.rows.length > 0 && width > 0) {
      table["!ref"] = XLSX.utils.encode_range({ s: { r: 0, c: 0 }, e: { r: item.rows.length - 1, c: width - 1 } });
    }
    XLSX.utils.book_append_sheet(book, table, item.name);
  }
  const out = XLSX.write(book, { type: "array", bookType: "xlsx" });
  if (out instanceof ArrayBuffer) return new Uint8Array(out);
  if (out instanceof Uint8Array) return new Uint8Array(out);
  return Uint8Array.from(out as number[]);
}

function storedZip(files: [string, string][]): Uint8Array {
  const chunks: Buffer[] = [];
  const central: Buffer[] = [];
  let offset = 0;
  for (const [name, text] of files) {
    const data = Buffer.from(text);
    const nameBuf = Buffer.from(name);
    const crc = crc32(data) >>> 0;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    chunks.push(local, nameBuf, data);
    const cen = Buffer.alloc(46);
    cen.writeUInt32LE(0x02014b50, 0);
    cen.writeUInt16LE(20, 4);
    cen.writeUInt16LE(20, 6);
    cen.writeUInt32LE(crc, 16);
    cen.writeUInt32LE(data.length, 20);
    cen.writeUInt32LE(data.length, 24);
    cen.writeUInt16LE(nameBuf.length, 28);
    cen.writeUInt32LE(offset, 42);
    central.push(cen, nameBuf);
    offset += local.length + nameBuf.length + data.length;
  }
  const centralBuf = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(centralBuf.length, 12);
  end.writeUInt32LE(offset, 16);
  return new Uint8Array(Buffer.concat([...chunks, centralBuf, end]));
}

function chartFirstWorkbook(): Uint8Array {
  return storedZip([
    ["[Content_Types].xml", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/chartsheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.chartsheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>`],
    ["_rels/.rels", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>`],
    ["xl/workbook.xml", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Chart1" sheetId="1" r:id="rId1"/><sheet name="Data" sheetId="2" r:id="rId2"/></sheets></workbook>`],
    ["xl/_rels/workbook.xml.rels", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet" Target="chartsheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>`],
    ["xl/chartsheets/sheet1.xml", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<chartsheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"></chartsheet>`],
    ["xl/worksheets/sheet2.xml", `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>name</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>只在第二张表</t></is></c></row></sheetData></worksheet>`],
  ]);
}

async function chooseBytes(wrapper: ReturnType<typeof dialog>, name: string, bytes: Uint8Array) {
  const input = wrapper.get("input[type='file']");
  const copy = new Uint8Array(bytes);
  const file = new File([copy.buffer as ArrayBuffer], name, { type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" });
  Object.defineProperty(input.element, "files", { value: { 0: file, length: 1, item: () => file } });
  await input.trigger("change");
  await flushPromises();
}

describe("xlsx import", () => {
  const rows = xlsxBytes([
    { name: "First", rows: [["name", "email", "note"], ["甲", "a@example.com", null], ["乙", "b@example.com", "备注"]] },
    { name: "Second", rows: [["name", "email", "note"], ["只在第二张表", "secret@example.com", "忽略"]] },
  ]);

  beforeEach(() => {
    invoke.mockReset();
    resetCatalog();
  });

  it("writes mapped rows from the first sheet only", async () => {
    invoke.mockResolvedValue({
      applied: [
        { id: "import-1", sql: "INSERT", label: "新增 1 行" },
        { id: "import-2", sql: "INSERT", label: "新增 1 行" },
      ],
      failed: null,
      pendingIds: [],
      rolledBack: false,
      rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。",
    });
    const wrapper = dialog();
    await chooseBytes(wrapper, "rows.xlsx", rows);
    expect((wrapper.get("input[value='xlsx']").element as HTMLInputElement).checked).toBe(true);
    expect(wrapper.text()).toContain("将写入 2 行");
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find((item) => item[0] === "grid_commit");
    const changes = call?.[1].request.changes as { values: { column: string; value: string | null }[] }[];
    expect(changes).toHaveLength(2);
    expect(changes[0]?.values).toEqual(expect.arrayContaining([
      { column: "name", value: "甲" },
      { column: "email", value: "a@example.com" },
      { column: "note", value: null },
    ]));
    expect(JSON.stringify(changes)).not.toContain("只在第二张表");
    expect(wrapper.get("[data-import-result]").text()).toContain("已写入 2 行");
    wrapper.unmount();
  });

  it("writes nothing when import is cancelled or the sheet cannot be mapped", async () => {
    const cancelled = dialog();
    await chooseBytes(cancelled, "rows.xlsx", rows);
    await cancelled.get(".btn").trigger("click");
    expect(cancelled.emitted("close")).toHaveLength(1);
    expect(invoke).not.toHaveBeenCalled();
    cancelled.unmount();

    const cases: { name: string; bytes: Uint8Array; reason: string }[] = [
      { name: "empty.xlsx", bytes: new Uint8Array(), reason: "xlsx 文件是空的。" },
      { name: "notes.xlsx", bytes: new TextEncoder().encode("hello"), reason: "第一个工作表不是表。" },
      { name: "dup.xlsx", bytes: xlsxBytes([{ name: "S", rows: [["email", "email"], ["甲", "乙"]] }]), reason: "xlsx 列名重复：email" },
      { name: "blank.xlsx", bytes: xlsxBytes([{ name: "S", rows: [["name", null], ["甲", "乙"]] }]), reason: "xlsx 缺少列名。" },
      { name: "chart.xlsx", bytes: chartFirstWorkbook(), reason: "第一个工作表不是表。" },
    ];
    for (const item of cases) {
      invoke.mockClear();
      const wrapper = dialog();
      await chooseBytes(wrapper, item.name, item.bytes);
      expect(wrapper.text()).toContain(item.reason);
      await wrapper.get(".btn.primary").trigger("click");
      expect(invoke).not.toHaveBeenCalled();
      wrapper.unmount();
    }
  });

  it("reports a failed xlsx row and does not call the import a full success", async () => {
    invoke.mockResolvedValue({
      applied: [{ id: "import-1", sql: "INSERT", label: "新增 1 行" }],
      failed: { id: "import-2", sql: "INSERT", label: "新增 1 行", message: "invalid input syntax for type integer" },
      pendingIds: ["import-2"],
      rolledBack: false,
      rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不撤销。",
    });
    const wrapper = dialog();
    await chooseBytes(wrapper, "rows.xlsx", rows);
    await wrapper.get(".btn.primary").trigger("click");
    await flushPromises();
    const text = wrapper.get("[data-import-result]").text();
    expect(text).toContain("第 2 行失败");
    expect(text).toContain("invalid input syntax for type integer");
    expect(text).toContain("不是全部成功");
    expect(text).not.toContain("已写入 2 行");
    wrapper.unmount();
  });

  it("reads an empty cell as null and keeps a literal backslash-N token", () => {
    const outcome = parseImportXlsx(xlsxBytes([{
      name: "S",
      rows: [["a", "b"], ["1", ""], ["2", null], ["3", "\\N"]],
    }]));
    expect(outcome).toEqual({
      ok: true,
      data: {
        columns: ["a", "b"],
        rows: [
          { a: "1", b: null },
          { a: "2", b: null },
          { a: "3", b: "\\N" },
        ],
      },
    });
  });
});
