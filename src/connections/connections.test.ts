import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import type { ConnectionApi, EnvTagInput, TestReport } from "./api";
import ConnectionsPage from "./ConnectionsPage.vue";
import { listPaletteConnections, MODULE_ID, replacePaletteConnections, setSqlitePicker } from "./index";
import { blankDraft, storedConfigHidesSecrets, toStoredConfig, validate, type EnvTag, type SavedConnection, type SecretInput } from "./model";

const secret: SecretInput = { password: "db-secret-alpha", sshPassword: "ssh-secret-beta", passphrase: "key-pass-gamma" };

function seedTags(): EnvTag[] {
  return [
    { id: "dev", label: "开发", tone: "dev", position: 0 },
    { id: "test", label: "测试", tone: "test", position: 1 },
    { id: "prod", label: "生产", tone: "prod", position: 2 },
    { id: "local", label: "本地", tone: "local", position: 3 },
  ];
}

function tagApi(tags: EnvTag[], extra: Partial<ConnectionApi> = {}): Pick<ConnectionApi, "listTags" | "saveTag" | "deleteTag"> {
  return {
    listTags: async () => tags.map((item) => ({ ...item })),
    saveTag: async (input: EnvTagInput) => {
      const label = input.label.trim();
      if (tags.some((item) => item.label === label && item.id !== input.id)) throw new Error("已经有同名环境标签");
      if (input.id) {
        const found = tags.find((item) => item.id === input.id);
        if (!found) throw new Error("找不到这个环境标签");
        found.label = label;
        found.tone = input.tone;
        return { ...found };
      }
      const created = { id: `tag-${tags.length + 1}`, label, tone: input.tone, position: tags.length };
      tags.push(created);
      return { ...created };
    },
    deleteTag: async (id: string) => {
      const index = tags.findIndex((item) => item.id === id);
      if (index >= 0) tags.splice(index, 1);
    },
    ...extra,
  };
}

function savedMysql(): SavedConnection {
  return {
    id: "atlas",
    name: "Atlas",
    engine: "mysql",
    env: "dev",
    host: "db.example.test",
    port: 3306,
    database: "app",
    username: "app_user",
    filePath: null,
    tls: { mode: "verify", serverName: "db.example.test", caMode: "system" },
    ssh: null,
    passwordSaved: true,
    sshPasswordSaved: false,
    sshHostKey: null,
  };
}

describe("connection model", () => {
  it("keeps mysql distinct from mariadb and leaves secrets out of sqlite config", () => {
    expect(MODULE_ID).toBe("connections");
    const mysql = toStoredConfig({ ...blankDraft("mysql"), host: "db.example.test", username: "app" }, "my");
    const maria = toStoredConfig({ ...blankDraft("mariadb"), host: "db.example.test", username: "app" }, "ma");
    expect(mysql.engine).toBe("mysql");
    expect(maria.engine).toBe("mariadb");
    expect(mysql.engine).not.toBe(maria.engine);
    const sqlite = toStoredConfig({ ...blankDraft("sqlite"), filePath: "C:/data/notes.sqlite", tlsMode: "verify", sshOn: true }, "file");
    expect(sqlite.tls).toBeNull();
    expect(sqlite.ssh).toBeNull();
    expect(storedConfigHidesSecrets(sqlite, secret)).toBe(true);
    expect(storedConfigHidesSecrets(mysql, secret)).toBe(true);
    expect(validate(blankDraft("sqlite")).filePath).toBe("请选择数据库文件");
  });
});

describe("connections page", () => {
  function pageApi(nextReport: () => TestReport): ConnectionApi {
    return {
      list: async () => [savedMysql()],
      ...tagApi(seedTags()),
      save: async (draft) => ({ connection: { ...toStoredConfig(draft, draft.id ?? "created"), passwordSaved: draft.savePassword }, note: null }),
      remove: async (id) => ({ connection: { ...savedMysql(), id }, databaseFileRemoved: false }),
      test: async () => nextReport(),
      open: async () => ({ report: nextReport(), connected: false, foreignKeys: null }),
      disconnect: async () => undefined,
      confirmHostKey: async () => undefined,
      forgetHostKey: async () => undefined,
    };
  }

  it("默认只显示直连必需项，高级选项默认折叠", async () => {
    replacePaletteConnections([]);
    setSqlitePicker(async () => "C:/data/notes.sqlite");
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api: pageApi(() => ({ status: "ok", stages: [] })) } });
    await flushPromises();

    // 卡片：名称、引擎、地址、状态清楚可读，徽章克制
    expect(listPaletteConnections().map((item) => item.label)).toEqual(["Atlas"]);
    const card = wrapper.get("article.conn-card");
    expect(card.text()).toContain("Atlas");
    expect(card.text()).toContain("MySQL");
    expect(card.text()).toContain("db.example.test:3306 / app");
    expect(card.text()).toContain("未连接");
    expect(card.text()).toContain("加密连接 · 核对证书");
    expect(card.text()).toContain("密码已保存");

    await wrapper.get("[data-action=conn-new]").trigger("click");
    // 默认表单：名称、引擎、地址/端口、账号在高级分区之外
    const advanced = wrapper.get('details[data-section="advanced"]');
    expect(advanced.attributes("open")).toBeUndefined();
    expect(advanced.text()).toContain("高级选项");
    expect(advanced.text()).toContain("TLS 加密、SSH 跳板机、环境标签");
    expect(wrapper.find('details[data-section="advanced"] [data-field="host"]').exists()).toBe(false);
    expect(wrapper.find('details[data-section="advanced"] input[name="cf-tls"]').exists()).toBe(true);
    expect(wrapper.find('details[data-section="advanced"] [data-action="env-manage"]').exists()).toBe(true);
    // 分阶段诊断不常驻：默认只有一个折叠的测试说明
    expect(wrapper.find(".test-steps").exists()).toBe(false);
    expect(wrapper.get("details.test-plan").attributes("open")).toBeUndefined();

    // SQLite：不展示 TLS/SSH，提示为直白文案
    await wrapper.get('input[value="sqlite"]').trigger("change");
    expect(wrapper.text()).toContain("本地文件，不需要 TLS 或 SSH");
    expect(wrapper.find('details[data-section="advanced"] [data-field="sshHost"]').exists()).toBe(false);
    wrapper.unmount();
    replacePaletteConnections([]);
  });

  it("校验失败时自动展开包含错误的高级分区", async () => {
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api: pageApi(() => ({ status: "ok", stages: [] })) } });
    await flushPromises();
    await wrapper.get("[data-action=conn-new]").trigger("click");
    await wrapper.get("[data-field=host]").setValue("db.example.test");
    await wrapper.get("[data-field=username]").setValue("app_user");
    await wrapper.get("[data-field=sshOn]").setValue(true);
    expect(wrapper.get('details[data-section="advanced"]').attributes("open")).toBeUndefined();
    await wrapper.get("[data-action=cf-test]").trigger("click");
    expect(wrapper.text()).toContain("请填写跳板机主机");
    expect(wrapper.get('details[data-section="advanced"]').attributes("open")).toBeDefined();
    wrapper.unmount();
  });

  it("测试失败时自动展开失败阶段，其余阶段收进折叠详情", async () => {
    const failure: TestReport = {
      status: "err",
      failStage: "加密连接并核对证书",
      hint: "没有改用 localhost，也没有关闭验证后重试。",
      stages: [
        { kind: "network", label: "连接数据库服务器", status: "ok", message: "db.example.test:5432" },
        { kind: "tls", label: "加密连接并核对证书", status: "err", message: "证书主机名不匹配" },
        { kind: "auth", label: "登录数据库", status: "skipped", message: "未执行" },
        { kind: "version", label: "读取数据库版本", status: "skipped", message: "未执行" },
      ],
    };
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api: pageApi(() => failure) } });
    await flushPromises();
    await wrapper.get("[data-action=conn-new]").trigger("click");
    await wrapper.get("[data-field=host]").setValue("db.example.test");
    await wrapper.get("[data-field=username]").setValue("app_user");
    // 手动展开高级分区后配置 SSH，TLS 说明文案直白
    const advanced = wrapper.get('details[data-section="advanced"]');
    (advanced.element as HTMLDetailsElement).open = true;
    await advanced.trigger("toggle");
    await wrapper.get("[data-field=sshOn]").setValue(true);
    await wrapper.get("[data-field=sshHost]").setValue("bastion.example.com");
    await wrapper.get("[data-field=sshUser]").setValue("jump");
    expect(wrapper.get("[data-tls-copy]").text()).toContain("证书仍按数据库主机名 db.example.test 校验");
    expect(wrapper.get("[data-tls-copy]").text()).toContain("不会改用 localhost");

    await wrapper.get("[data-action=cf-test]").trigger("click");
    await flushPromises();
    const panel = wrapper.get(".test-panel");
    expect(panel.classes()).toContain("err");
    expect(panel.text()).toContain("连接失败：加密连接并核对证书");
    expect(wrapper.get("[data-test-fail]").text()).toBe("证书主机名不匹配");
    expect(panel.text()).toContain("没有改用 localhost，也没有关闭验证后重试。");
    // 其余阶段不堆砌：完整阶段列表在默认折叠的详情里
    const all = wrapper.get("details.test-all");
    expect(all.attributes("open")).toBeUndefined();
    expect(all.text()).toContain("登录数据库");
    wrapper.unmount();
  });

  it("测试成功时只给一句话摘要，不罗列各阶段", async () => {
    const success: TestReport = {
      status: "ok",
      summary: "PostgreSQL 16.4",
      stages: [
        { kind: "network", label: "连接数据库服务器", status: "ok", message: "db.example.test:5432" },
        { kind: "tls", label: "加密连接并核对证书", status: "ok", message: "证书有效" },
        { kind: "auth", label: "登录数据库", status: "ok", message: "已登录" },
        { kind: "version", label: "读取数据库版本", status: "ok", message: "PostgreSQL 16.4" },
      ],
    };
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api: pageApi(() => success) } });
    await flushPromises();
    await wrapper.get("[data-action=conn-new]").trigger("click");
    await wrapper.get("[data-field=host]").setValue("db.example.test");
    await wrapper.get("[data-field=username]").setValue("app_user");
    await wrapper.get("[data-action=cf-test]").trigger("click");
    await flushPromises();
    const panel = wrapper.get(".test-panel");
    expect(panel.classes()).toContain("ok");
    expect(panel.get(".test-line").text()).toBe("连接成功 · PostgreSQL 16.4");
    expect(wrapper.find(".test-steps").exists()).toBe(false);
    expect(panel.text()).not.toContain("登录数据库");
    wrapper.unmount();
  });

  it("删除确认逐条列出会删与不会删的内容", async () => {
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api: pageApi(() => ({ status: "ok", stages: [] })) } });
    await flushPromises();
    await wrapper.get("[data-action=conn-menu]").trigger("click");
    await wrapper.get("[data-action=conn-delete]").trigger("click");
    expect(wrapper.text()).toContain("会删除：本机保存的这条连接配置，以及系统凭据库里保存的对应密码");
    expect(wrapper.text()).toContain("不会删除：数据库本身和其中的任何数据");
    await wrapper.get("[data-action=confirm-delete]").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("数据库和其中的数据不受影响");
    wrapper.unmount();
  });

  it("lets the user add, rename, and delete environment tags", async () => {
    const tags = seedTags();
    const api: ConnectionApi = {
      list: async () => [savedMysql()],
      ...tagApi(tags),
      save: async (draft) => ({ connection: { ...toStoredConfig(draft, draft.id ?? "created"), passwordSaved: draft.savePassword }, note: null }),
      remove: async (id) => ({ connection: { ...savedMysql(), id }, databaseFileRemoved: false }),
      test: async () => ({ status: "ok", stages: [] }),
      open: async () => ({ report: { status: "ok", stages: [] }, connected: true, foreignKeys: null }),
      disconnect: async () => undefined,
      confirmHostKey: async () => undefined,
      forgetHostKey: async () => undefined,
    };
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api } });
    await flushPromises();
    expect(wrapper.text()).toContain("开发");

    await wrapper.get("[data-action=conn-new]").trigger("click");
    await wrapper.get("[data-action=env-manage]").trigger("click");
    expect((wrapper.get("[data-field=env-label]").element as HTMLInputElement).value).toBe("开发");
    await wrapper.get("[data-field=env-label]").setValue("开发环境");
    await wrapper.get("[data-action=env-save]").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("开发环境");
    expect(tags.find((item) => item.id === "dev")?.label).toBe("开发环境");

    await wrapper.get("[data-action=env-add]").trigger("click");
    await wrapper.get("[data-field=env-label]").setValue("预发");
    await wrapper.get("[data-action=env-save]").trigger("click");
    await flushPromises();
    const created = wrapper.findAll(".env-pick label").find((item) => item.text().includes("预发"));
    expect(created?.classes()).toContain("on");

    await wrapper.get("[data-field=env-label]").setValue("灰度");
    await wrapper.get("[data-action=env-save]").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("灰度");
    expect(wrapper.text()).not.toContain("预发");

    await wrapper.get("[data-field=env-label]").setValue("开发环境");
    await wrapper.get("[data-action=env-save]").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("已经有同名环境标签");

    await wrapper.get("[data-field=env-label]").setValue("灰度");
    await wrapper.get("[data-action=env-delete]").trigger("click");
    expect(wrapper.get("[data-action=env-delete]").text()).toBe("确认删除");
    await wrapper.get("[data-action=env-delete]").trigger("click");
    await flushPromises();
    expect(wrapper.text()).not.toContain("灰度");
    expect(wrapper.get(".env-pick label.on").text()).toContain("无标签");
    wrapper.unmount();
  });
});
