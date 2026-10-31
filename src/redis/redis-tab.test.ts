import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import RedisTab from "./RedisTab.vue";
import type { RedisApi, RedisKeyDetail, ScanPage } from "./api";

function page(cursor: number, keys: ScanPage["keys"]): ScanPage {
  return { cursor, done: cursor === 0, keys };
}

const detailOf = (keyType: string, value: RedisKeyDetail["value"], ttlMs: number | null = -1): RedisKeyDetail => ({ keyType, ttlMs, value });

function makeApi(overrides: Partial<RedisApi> = {}): RedisApi {
  return {
    scanKeys: vi.fn(async () => page(0, [
      { name: "rillsql:string:welcome", keyType: "string", ttlMs: -1 },
      { name: "rillsql:zset:rank", keyType: "zset", ttlMs: 1_800_000 },
      { name: "rillsql:string:temp-token", keyType: "string", ttlMs: 3_600_000 },
    ])),
    readKey: vi.fn(async (_connectionId: string, key: string) => detailOf("string", { kind: "string", text: `值:${key}` })),
    setTtl: vi.fn(async () => ({ applied: 1 })),
    deleteKeys: vi.fn(async () => ({ deleted: 1 })),
    renameKey: vi.fn(async () => ({ renamed: true })),
    exec: vi.fn(async (_connectionId: string, command: string): Promise<{ command: string; reply: { kind: "bulk" | "status"; text: string | null; int: number | null; items: [] } }> => ({
      command,
      reply:
        command === "GET"
          ? { kind: "bulk", text: "PONG-like", int: null, items: [] }
          : { kind: "status", text: "OK", int: null, items: [] },
    })),
    ...overrides,
  };
}

async function mountTab(api: RedisApi, initialKey?: string) {
  const wrapper = mount(RedisTab, {
    props: {
      tabId: "keys:cache",
      connectionId: "cache",
      connectionName: "缓存",
      database: "0",
      status: "connected",
      initialKey,
      api,
    },
  });
  await flushPromises();
  return wrapper;
}

describe("redis tab (keys workspace)", () => {
  it("loads the first scan page with type badges and ttl text", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    const rows = wrapper.findAll("[data-redis-keys] [data-key]");
    expect(rows).toHaveLength(3);
    expect(rows[0]!.attributes("data-type")).toBe("string");
    expect(rows[1]!.attributes("data-type")).toBe("zset");
    expect(rows[1]!.text()).toContain("30 分 0 秒");
    expect(api.scanKeys).toHaveBeenCalledWith("cache", 0, null);
  });

  it("passes the pattern to scan and restarts from cursor zero when filtering", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    await wrapper.find("[data-field=redis-pattern]").setValue("rillsql:string:*");
    await wrapper.find("[data-action=redis-apply-pattern]").trigger("submit");
    await flushPromises();
    expect(api.scanKeys).toHaveBeenLastCalledWith("cache", 0, "rillsql:string:*");
  });

  it("paginates with the returned cursor for the next scan call", async () => {
    let call = 0;
    const api = makeApi({
      scanKeys: vi.fn(async (_id: string, cursorArg: number) => {
        void cursorArg;
        call += 1;
        return call === 1
          ? page(42, [{ name: "a", keyType: "string", ttlMs: -1 }])
          : page(0, [{ name: "b", keyType: "hash", ttlMs: -1 }]);
      }),
    });
    const wrapper = await mountTab(api);
    expect(api.scanKeys).toHaveBeenNthCalledWith(1, "cache", 0, null);
    await wrapper.find("[data-action=redis-more]").trigger("click");
    await flushPromises();
    expect(api.scanKeys).toHaveBeenNthCalledWith(2, "cache", 42, null);
    expect(wrapper.findAll("[data-redis-keys] [data-key]")).toHaveLength(2);
  });

  it("renders the value viewer for the selected key and updates ttl", async () => {
    const api = makeApi({
      readKey: vi.fn(async () => detailOf("hash", { kind: "hash", entries: [{ field: "customer", value: "林夏" }] }, 3_600_000)),
    });
    const wrapper = await mountTab(api);
    await wrapper.find('[data-key="rillsql:zset:rank"]').trigger("click");
    await flushPromises();
    expect(api.readKey).toHaveBeenCalledWith("cache", "rillsql:zset:rank");
    expect(wrapper.find("[data-value-kind=hash]").exists()).toBe(true);
    expect(wrapper.find("[data-redis-key-title]").text()).toBe("rillsql:zset:rank");
    expect(wrapper.text()).toContain("TTL：1 时 0 分");
    // TTL 编辑：3600 秒 → EXPIRE；清空 → PERSIST。
    await wrapper.find("[data-field=redis-ttl]").setValue("120");
    await wrapper.find("[data-action=redis-set-ttl]").trigger("submit");
    await flushPromises();
    expect(api.setTtl).toHaveBeenCalledWith("cache", "rillsql:zset:rank", 120);
    await wrapper.find("[data-field=redis-ttl]").setValue("");
    await wrapper.find("[data-action=redis-set-ttl]").trigger("submit");
    await flushPromises();
    expect(api.setTtl).toHaveBeenLastCalledWith("cache", "rillsql:zset:rank", null);
  });

  it("requires confirmation for key deletion and cancels without any call", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    await wrapper.find('[data-key="rillsql:string:temp-token"]').trigger("click");
    await flushPromises();
    await wrapper.find("[data-action=redis-delete]").trigger("click");
    expect(wrapper.find("[data-redis-confirm-prompt]").text()).toContain("rillsql:string:temp-token");
    // 取消：不发 DEL。
    await wrapper.find("[data-action=redis-confirm-cancel]").trigger("click");
    await flushPromises();
    expect(api.deleteKeys).not.toHaveBeenCalled();
    expect(wrapper.findAll("[data-menu=confirm]")).toHaveLength(0);
    // 再来一次并确认：confirmed=true 才发。
    await wrapper.find("[data-action=redis-delete]").trigger("click");
    await wrapper.find("[data-action=redis-confirm-ok]").trigger("click");
    await flushPromises();
    expect(api.deleteKeys).toHaveBeenCalledWith("cache", ["rillsql:string:temp-token"], true);
  });

  it("renames a key through the two-step form with confirmation", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    await wrapper.find('[data-key="rillsql:string:welcome"]').trigger("click");
    await flushPromises();
    await wrapper.find("[data-action=redis-rename]").trigger("click");
    await wrapper.find("[data-field=redis-rename-to]").setValue("rillsql:string:renamed");
    await wrapper.find("[data-action=redis-rename-confirm]").trigger("submit");
    await wrapper.find("[data-action=redis-confirm-ok]").trigger("click");
    await flushPromises();
    expect(api.renameKey).toHaveBeenCalledWith("cache", "rillsql:string:welcome", "rillsql:string:renamed", true);
  });

  it("runs readonly commands directly and dangerous ones only after confirmation", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    // 只读直通：GET 不需要确认，confirmed=false。
    await wrapper.find("[data-field=redis-command]").setValue("GET rillsql:string:welcome");
    await wrapper.find("[data-action=redis-exec]").trigger("submit");
    await flushPromises();
    expect(api.exec).toHaveBeenCalledWith("cache", "GET", ["rillsql:string:welcome"], false);
    expect(wrapper.find("[data-reply-kind=bulk]").exists()).toBe(true);

    // 写删类：先弹确认，取消不发；确认才发 confirmed=true。
    await wrapper.find("[data-field=redis-command]").setValue("FLUSHDB");
    await wrapper.find("[data-action=redis-exec]").trigger("submit");
    await flushPromises();
    expect(api.exec).toHaveBeenCalledTimes(1);
    expect(wrapper.find("[data-redis-confirm-prompt]").text()).toContain("清空当前库");
    await wrapper.find("[data-action=redis-confirm-cancel]").trigger("click");
    expect(api.exec).toHaveBeenCalledTimes(1);

    await wrapper.find("[data-action=redis-exec]").trigger("submit");
    await wrapper.find("[data-action=redis-confirm-ok]").trigger("click");
    await flushPromises();
    expect(api.exec).toHaveBeenLastCalledWith("cache", "FLUSHDB", [], true);
  });

  it("refuses KEYS client-side with a scan hint", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api);
    await wrapper.find("[data-field=redis-command]").setValue("KEYS *");
    await wrapper.find("[data-action=redis-exec]").trigger("submit");
    await flushPromises();
    expect(api.exec).not.toHaveBeenCalled();
    expect(wrapper.find("[data-redis-cmd-error]").text()).toContain("已禁用 KEYS");
  });

  it("keeps the initial key selected when opened from the tree", async () => {
    const api = makeApi();
    const wrapper = await mountTab(api, "rillsql:zset:rank");
    expect(wrapper.find("[data-redis-key-title]").text()).toBe("rillsql:zset:rank");
    expect(api.readKey).toHaveBeenCalledWith("cache", "rillsql:zset:rank");
  });
});
