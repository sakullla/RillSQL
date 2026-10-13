import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import type { ConnectionApi, TestReport } from "./api";
import ConnectionsPage from "./ConnectionsPage.vue";
import { listPaletteConnections, MODULE_ID, replacePaletteConnections, setSqlitePicker } from "./index";
import { blankDraft, storedConfigHidesSecrets, toStoredConfig, validate, type SavedConnection, type SecretInput } from "./model";

const secret: SecretInput = { password: "db-secret-alpha", sshPassword: "ssh-secret-beta", passphrase: "key-pass-gamma" };

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
});
