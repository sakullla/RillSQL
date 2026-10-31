import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import RedisValueView from "./RedisValueView.vue";
import type { RedisValuePayload } from "../redis/api";

describe("redis value viewer (per-type rendering, no DataGrid)", () => {
  it("renders a string value as plain text", () => {
    const wrapper = mount(RedisValueView, { props: { value: { kind: "string", text: "澄库 Redis 夹具 · 中文值" } } });
    expect(wrapper.find("[data-value-kind=string]").exists()).toBe(true);
    expect(wrapper.find("pre.rv-text").text()).toBe("澄库 Redis 夹具 · 中文值");
  });

  it("renders a hash as a field/value table", () => {
    const value: RedisValuePayload = {
      kind: "hash",
      entries: [
        { field: "status", value: "paid" },
        { field: "customer", value: "林夏" },
      ],
    };
    const wrapper = mount(RedisValueView, { props: { value } });
    const rows = wrapper.findAll("tbody tr");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.text()).toContain("status");
    expect(rows[0]!.text()).toContain("paid");
    expect(rows[1]!.text()).toContain("林夏");
  });

  it("renders a list with indexes and a set without", () => {
    const list = mount(RedisValueView, { props: { value: { kind: "list", items: ["任务A", "任务B"] } } });
    expect(list.find("[data-value-kind=list]").exists()).toBe(true);
    expect(list.findAll("ol.rv-list li")).toHaveLength(2);
    expect(list.text()).toContain("任务A");

    const set = mount(RedisValueView, { props: { value: { kind: "set", items: ["紧急", "加急"] } } });
    expect(set.find("[data-value-kind=set]").exists()).toBe(true);
    expect(set.findAll("ul.rv-list li")).toHaveLength(2);
  });

  it("renders a zset with member and score columns", () => {
    const value: RedisValuePayload = {
      kind: "zset",
      entries: [
        { member: "张伟", score: "90" },
        { member: "林夏", score: "85" },
      ],
    };
    const wrapper = mount(RedisValueView, { props: { value } });
    const rows = wrapper.findAll("tbody tr");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.text()).toContain("张伟");
    expect(rows[0]!.text()).toContain("90");
    expect(rows[1]!.text()).toContain("林夏");
  });

  it("renders stream messages with ids and field pairs", () => {
    const value: RedisValuePayload = {
      kind: "stream",
      messages: [{ id: "1698751000000-0", fields: [{ field: "event", value: "order.created" }, { field: "operator", value: "管理员" }] }],
    };
    const wrapper = mount(RedisValueView, { props: { value } });
    expect(wrapper.find("article.rv-message").exists()).toBe(true);
    expect(wrapper.find("article.rv-message header").text()).toBe("1698751000000-0");
    const pairs = wrapper.findAll("article.rv-message dl > *");
    expect(pairs).toHaveLength(4);
    expect(wrapper.text()).toContain("order.created");
    expect(wrapper.text()).toContain("管理员");
  });

  it("shows an explicit empty state for missing keys or empty collections", () => {
    const none = mount(RedisValueView, { props: { value: { kind: "none" } } });
    expect(none.find(".rv-empty").text()).toContain("键不存在");
    const emptyList = mount(RedisValueView, { props: { value: { kind: "list", items: [] } } });
    expect(emptyList.find(".rv-empty").text()).toContain("没有内容");
  });
});
