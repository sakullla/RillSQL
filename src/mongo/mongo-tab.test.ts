import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import MongoDocsTab from "./MongoDocsTab.vue";
import type { MongoApi, MongoDocumentsPage } from "./api";

const firstDoc = {
  idJson: '{"$oid":"65f2c1e4a1b2c3d4e5f60718"}',
  json: '{"_id":{"$oid":"65f2c1e4a1b2c3d4e5f60718"},"status":"paid","customer":"林夏","items":[{"sku":"A-1","qty":2}]}',
};
const secondDoc = {
  idJson: '{"$oid":"65f2c1e4a1b2c3d4e5f60719"}',
  json: '{"_id":{"$oid":"65f2c1e4a1b2c3d4e5f60719"},"status":"pending","customer":"周宁"}',
};

function pageOf(documents: typeof firstDoc[], total: number): MongoDocumentsPage {
  return { total, documents };
}

function makeApi(overrides: Partial<MongoApi> = {}): MongoApi {
  return {
    listDatabases: vi.fn(async () => []),
    listCollections: vi.fn(async () => []),
    findDocuments: vi.fn(async () => pageOf([firstDoc, secondDoc], 2)),
    readDocument: vi.fn(async () => ({ json: firstDoc.json })),
    replaceDocument: vi.fn(async () => ({ matched: 1, modified: 1 })),
    insertDocument: vi.fn(async () => ({ insertedIdJson: '{"$oid":"65f2c1e4a1b2c3d4e5f60f00"}' })),
    deleteDocument: vi.fn(async () => ({ deleted: 1 })),
    ...overrides,
  };
}

async function mountTab(api: MongoApi) {
  const wrapper = mount(MongoDocsTab, {
    props: {
      tabId: "docs:mongo:rillsql:orders",
      connectionId: "mongo",
      connectionName: "文档库",
      database: "rillsql",
      collection: "orders",
      status: "connected",
      api,
    },
  });
  await flushPromises();
  return wrapper;
}

describe("mongo docs tab (collection workspace)", () => {
  it("loads the first page with _id and top-level fields as columns", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    expect(api.findDocuments).toHaveBeenCalledWith("mongo", "rillsql", "orders", 0, 20, null);
    const header = wrapper.findAll("thead th").map((th) => th.attributes("data-column")).filter(Boolean);
    expect(header).toEqual(["_id", "status", "customer", "items"]);
    expect(wrapper.find('[data-cell="customer"]').text()).toBe("林夏");
    expect(wrapper.find('[data-cell="items"]').text()).toBe('[{"sku":"A-1","qty":2}]');
    expect(wrapper.find("[data-mongo-count]").text()).toContain("约 2 篇");
  });

  it("applies a json filter and shows the located syntax error from the backend", async () => {
    const api = makeApi({
      findDocuments: vi.fn(async () => {
        throw '过滤语法错误: EOF while parsing a value at line 1 column 13';
      }),
    });
    const wrapper = await mountTab(api);
    await wrapper.find("[data-field=mongo-filter]").setValue('{"status": }');
    await wrapper.find("[data-action=mongo-apply-filter]").trigger("submit");
    await flushPromises();
    // 语法定位由服务端给出（本地不做拦截，保留 line/column 提示）。
    const error = wrapper.find("[data-mongo-filter-error]");
    expect(error.exists()).toBe(true);
    expect(error.text()).toContain("line 1 column 13");
  });

  it("sends invalid filter syntax to the backend and surfaces its located error", async () => {
    const api = makeApi({
      findDocuments: vi.fn(async (_id: string, _db: string, _coll: string, _skip: number, _limit: number, filterJson: string | null) => {
        if (filterJson === '{"status": active}') {
          throw '过滤语法错误: expected value at line 1 column 12';
        }
        return pageOf([firstDoc, secondDoc], 2);
      }),
    });
    const wrapper = await mountTab(api);
    await wrapper.find("[data-field=mongo-filter]").setValue('{"status": active}');
    await wrapper.find("[data-action=mongo-apply-filter]").trigger("submit");
    await flushPromises();
    expect(api.findDocuments).toHaveBeenCalledTimes(2);
    expect(wrapper.find("[data-mongo-filter-error]").text()).toContain("line 1 column 12");
  });

  it("passes a valid filter through and clears it afterwards", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    await wrapper.find("[data-field=mongo-filter]").setValue('{"status":"paid"}');
    await wrapper.find("[data-action=mongo-apply-filter]").trigger("submit");
    await flushPromises();
    expect(api.findDocuments).toHaveBeenLastCalledWith("mongo", "rillsql", "orders", 0, 20, '{"status":"paid"}');
    expect(wrapper.find("[data-mongo-count]").text()).toContain("匹配");
    await wrapper.find("[data-action=mongo-clear-filter]").trigger("click");
    await flushPromises();
    expect(api.findDocuments).toHaveBeenLastCalledWith("mongo", "rillsql", "orders", 0, 20, null);
  });

  it("paginates forward and back with skip stepping", async () => {
    const api = makeApi({ findDocuments: vi.fn(async () => pageOf([firstDoc], 40)) });
    const wrapper = await mountTab(api);
    await wrapper.find("[data-action=mongo-next]").trigger("click");
    await flushPromises();
    expect(api.findDocuments).toHaveBeenLastCalledWith("mongo", "rillsql", "orders", 20, 20, null);
    await wrapper.find("[data-action=mongo-prev]").trigger("click");
    await flushPromises();
    expect(api.findDocuments).toHaveBeenLastCalledWith("mongo", "rillsql", "orders", 0, 20, null);
  });

  it("views a document as pretty json in the value viewer", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    await wrapper.findAll("[data-action=mongo-view]")[0]!.trigger("click");
    await flushPromises();
    const view = wrapper.find("[data-value-kind=json]");
    expect(view.exists()).toBe(true);
    expect(view.text()).toContain("林夏");
    expect(view.text()).toContain('"$oid"');
  });

  it("intercepts invalid json on replace before any ipc, then saves a valid document", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    await wrapper.findAll("[data-action=mongo-edit]")[0]!.trigger("click");
    await wrapper.find("[data-field=mongo-editor]").setValue('{"status": "paid"');
    await wrapper.find("[data-action=mongo-editor-save]").trigger("submit");
    await flushPromises();
    expect(api.replaceDocument).not.toHaveBeenCalled();
    expect(wrapper.find("[data-mongo-editor-error]").text()).toContain("JSON 语法错误");

    await wrapper.find("[data-field=mongo-editor]").setValue('{"status":"shipped","customer":"林夏"}');
    await wrapper.find("[data-action=mongo-editor-save]").trigger("submit");
    await flushPromises();
    expect(api.replaceDocument).toHaveBeenCalledWith(
      "mongo",
      "rillsql",
      "orders",
      firstDoc.idJson,
      '{"status":"shipped","customer":"林夏"}',
    );
    expect(wrapper.find("[data-mongo-note]").text()).toContain("整体覆盖保存");
  });

  it("surfaces the backend bson validation error when the server rejects a document", async () => {
    const api = makeApi({
      replaceDocument: vi.fn(async () => {
        throw '过滤里的扩展 JSON 值不合法: $numberLong must be a string';
      }),
    });
    const wrapper = await mountTab(api);
    await wrapper.findAll("[data-action=mongo-edit]")[0]!.trigger("click");
    await wrapper.find("[data-field=mongo-editor]").setValue('{"count":{"$numberLong":5}}');
    await wrapper.find("[data-action=mongo-editor-save]").trigger("submit");
    await flushPromises();
    expect(api.replaceDocument).toHaveBeenCalledTimes(1);
    expect(wrapper.find("[data-mongo-editor-error]").text()).toContain("$numberLong");
  });

  it("inserts a new document with validation and reports the server _id", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    await wrapper.find("[data-action=mongo-insert]").trigger("click");
    await wrapper.find("[data-field=mongo-editor]").setValue('{"status":"new","备注":"加急"}');
    await wrapper.find("[data-action=mongo-editor-save]").trigger("submit");
    await flushPromises();
    expect(api.insertDocument).toHaveBeenCalledWith("mongo", "rillsql", "orders", '{"status":"new","备注":"加急"}');
    expect(wrapper.find("[data-mongo-note]").text()).toContain('{"$oid":"65f2c1e4a1b2c3d4e5f60f00"}');
  });

  it("requires confirmation for deletion, cancels without ipc, then reports the deleted count", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    await wrapper.findAll("[data-action=mongo-delete]")[1]!.trigger("click");
    expect(wrapper.find("[data-mongo-confirm-prompt]").text()).toContain(secondDoc.idJson);
    await wrapper.find("[data-action=mongo-confirm-cancel]").trigger("click");
    await flushPromises();
    expect(api.deleteDocument).not.toHaveBeenCalled();

    await wrapper.findAll("[data-action=mongo-delete]")[1]!.trigger("click");
    await wrapper.find("[data-action=mongo-confirm-ok]").trigger("click");
    await flushPromises();
    expect(api.deleteDocument).toHaveBeenCalledWith("mongo", "rillsql", "orders", secondDoc.idJson, true);
    expect(wrapper.find("[data-mongo-note]").text()).toContain("已删除 1 篇文档");
  });
});
