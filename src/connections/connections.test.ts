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
  it("matches the connection page rules for tls, stages, and delete", async () => {
    replacePaletteConnections([]);
    setSqlitePicker(async () => "C:/data/notes.sqlite");
    const reports: TestReport[] = [
      {
        status: "err",
        failStage: "TLS 握手与证书验证",
        hint: "没有改用 localhost，也没有关闭验证后重试。",
        stages: [
          { kind: "network", label: "网络连接", status: "ok", message: "db.example.test:5432" },
          { kind: "tls", label: "TLS 握手与证书验证", status: "err", message: "证书主机名不匹配" },
          { kind: "auth", label: "数据库认证", status: "skipped", message: "未执行" },
          { kind: "version", label: "读取服务器版本", status: "skipped", message: "未执行" },
        ],
      },
    ];
    const api: ConnectionApi = {
      list: async () => [savedMysql()],
      ...tagApi(seedTags()),
      save: async (draft) => ({ connection: { ...toStoredConfig(draft, draft.id ?? "created"), passwordSaved: draft.savePassword }, note: null }),
      remove: async (id) => ({ connection: { ...savedMysql(), id }, databaseFileRemoved: false }),
      test: async () => reports[0],
      open: async () => ({ report: reports[0], connected: false, foreignKeys: null }),
      disconnect: async () => undefined,
      confirmHostKey: async () => undefined,
      forgetHostKey: async () => undefined,
    };
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api } });
    await flushPromises();
    expect(listPaletteConnections().map((item) => item.label)).toEqual(["Atlas"]);
    expect(wrapper.text()).toContain("MySQL");
    expect(wrapper.text()).toContain("密码在系统凭据库");

    await wrapper.get("[data-action=conn-new]").trigger("click");
    await wrapper.get('input[value="sqlite"]').trigger("change");
    expect(wrapper.text()).toContain("不需要 TLS");
    expect(wrapper.text()).not.toContain("TLS / SSL");

    await wrapper.get('input[value="postgres"]').trigger("change");
    await wrapper.get("[data-field=host]").setValue("db.example.test");
    await wrapper.get("[data-field=username]").setValue("app_user");
    await wrapper.get("[data-section=ssh]").trigger("click");
    await wrapper.get("[data-field=sshOn]").setValue(true);
    await wrapper.get("[data-field=sshHost]").setValue("bastion.example.com");
    await wrapper.get("[data-field=sshUser]").setValue("jump");
    await wrapper.get("[data-section=tls]").trigger("click");
    expect(wrapper.get("[data-tls-copy]").text()).toContain("证书仍按数据库主机名 db.example.test 校验");
    expect(wrapper.get("[data-tls-copy]").text()).toContain("127.0.0.1");
    expect(wrapper.get("[data-tls-copy]").text()).toContain("不会改用 localhost");

    await wrapper.get("[data-action=cf-test]").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("未执行");
    expect(wrapper.text()).toContain("证书主机名不匹配");

    wrapper.unmount();
    const page = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api } });
    await flushPromises();
    await page.get("[data-action=conn-menu]").trigger("click");
    await page.get("[data-action=conn-delete]").trigger("click");
    expect(page.text()).toContain("不会");
    expect(page.text()).toContain("数据库或其中的任何数据");
    page.unmount();
    replacePaletteConnections([]);
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
