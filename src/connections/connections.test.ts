import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import type { ConnectionApi, EnvTagInput, TestReport } from "./api";
import ConnectionsPage from "./ConnectionsPage.vue";
import { listPaletteConnections, MODULE_ID, replacePaletteConnections, setSqlitePicker } from "./index";
import { blankDraft, storedConfigHidesSecrets, toStoredConfig, validate, type EnvTag, type SavedConnection, type SecretInput } from "./model";
import { parseConnectionUrl } from "./url";

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

function savedSqlite(): SavedConnection {
  return {
    id: "notes",
    name: "本地笔记",
    engine: "sqlite",
    env: "local",
    host: null,
    port: null,
    database: null,
    username: null,
    filePath: "C:/data/notes.sqlite",
    tls: null,
    ssh: null,
    passwordSaved: false,
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

describe("parseConnectionUrl", () => {
  it("解析 postgres / mysql 标准连接串", () => {
    const pg = parseConnectionUrl("postgres://app_user:secret@db.example.internal:5432/appdb");
    expect(pg).toEqual({ ok: true, engine: "postgres", host: "db.example.internal", port: "5432", database: "appdb", username: "app_user", password: "secret", tlsMode: null });
    const my = parseConnectionUrl("MYSQL://root@127.0.0.1:3307/inventory");
    expect(my).toEqual({ ok: true, engine: "mysql", host: "127.0.0.1", port: "3307", database: "inventory", username: "root", password: "", tlsMode: null });
  });

  it("解码密码里的百分号编码，畸形百分号按原文保留", () => {
    const decoded = parseConnectionUrl("postgres://u:p%40ss%21@h/db");
    expect(decoded.ok && decoded.password).toBe("p@ss!");
    const raw = parseConnectionUrl("postgres://u:pa%zz@h/db");
    expect(raw.ok && raw.password).toBe("pa%zz");
  });

  it("缺端口时取引擎默认端口，IPv6 主机去括号", () => {
    const pg = parseConnectionUrl("postgres://u@h/db");
    expect(pg.ok && pg.port).toBe("5432");
    const maria = parseConnectionUrl("mariadb://u@h/db");
    expect(maria.ok && maria.port).toBe("3306");
    const v6 = parseConnectionUrl("postgres://u@[::1]/db");
    expect(v6.ok && v6.host).toBe("::1");
  });

  it("sslmode 映射 TLS 档位，未出现时不覆盖", () => {
    const disable = parseConnectionUrl("postgres://u@h/db?sslmode=disable");
    expect(disable.ok && disable.tlsMode).toBe("off");
    const prefer = parseConnectionUrl("postgres://u@h/db?sslmode=prefer");
    expect(prefer.ok && prefer.tlsMode).toBe("require");
    const verify = parseConnectionUrl("postgres://u@h/db?sslmode=verify-full");
    expect(verify.ok && verify.tlsMode).toBe("verify");
    const verifyCa = parseConnectionUrl("postgres://u@h/db?sslmode=verify-ca");
    expect(verifyCa.ok && verifyCa.tlsMode).toBe("verify");
    const absent = parseConnectionUrl("mysql://u@h/db?ssl-mode=REQUIRED");
    expect(absent.ok && absent.tlsMode).toBe(null);
    const plain = parseConnectionUrl("postgres://u@h/db");
    expect(plain.ok && plain.tlsMode).toBe(null);
  });

  it("不支持的前缀与残缺串给出中文原因", () => {
    for (const text of ["sqlite:C:/data/notes.db", "jdbc:postgresql://h/db", "postgres:h", "   "]) {
      expect(parseConnectionUrl(text)).toEqual({ ok: false, reason: expect.stringContaining("只支持") });
    }
    const noHost = parseConnectionUrl("postgres://");
    expect(noHost.ok).toBe(false);
    expect(!noHost.ok && noHost.reason).toContain("主机");
    const badPort = parseConnectionUrl("postgres://h:notaport/db");
    expect(badPort.ok).toBe(false);
    expect(!badPort.ok && badPort.reason).toContain("格式");
  });
});

describe("connections page", () => {
  function pageApi(nextReport: () => TestReport, list: SavedConnection[] = [savedMysql()]): ConnectionApi {
    return {
      list: async () => list,
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
    expect(card.find(".cc-addr").text()).toBe("db.example.test:3306");
    expect(card.find(".cc-endpoint small").text()).toBe("app");
    expect(card.text()).toContain("未连接");
    expect(card.text()).toContain("TLS · 核对证书");
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

  it("新建时粘贴连接串一键填充，切换引擎且密码仅本次使用", async () => {
    replacePaletteConnections([]);
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api: pageApi(() => ({ status: "ok", stages: [] })) } });
    await flushPromises();
    await wrapper.get("[data-action=conn-new]").trigger("click");

    // 折叠入口默认收起，只对非 SQLite 引擎显示
    const paste = wrapper.get('details[data-section="paste-url"]');
    expect(paste.attributes("open")).toBeUndefined();
    expect(paste.text()).toContain("从连接串粘贴");

    await wrapper.get("[data-field=url-paste]").setValue("mysql://app_user:db-secret@db.example.test/inventory?sslmode=disable");
    await wrapper.get("[data-action=cf-parse-url]").trigger("click");

    expect((wrapper.get('input[value="mysql"]').element as HTMLInputElement).checked).toBe(true);
    expect((wrapper.get("[data-field=host]").element as HTMLInputElement).value).toBe("db.example.test");
    expect((wrapper.get("[data-field=port]").element as HTMLInputElement).value).toBe("3306");
    expect((wrapper.get("[data-field=username]").element as HTMLInputElement).value).toBe("app_user");
    expect((wrapper.get("[data-field=password]").element as HTMLInputElement).value).toBe("db-secret");
    // 密码不自动勾选保存：只进本次会话，与系统凭据库语义对齐
    expect((wrapper.get("[data-field=savePassword]").element as HTMLInputElement).checked).toBe(false);
    expect((wrapper.find('input[name="cf-tls"][value="off"]').element as HTMLInputElement).checked).toBe(true);
    const note = wrapper.get(".paste-note");
    expect(note.text()).toContain("已把引擎切换为 MySQL");
    expect(note.text()).toContain("密码已填入，仅本次使用；勾选保存才会写入系统凭据库");
    expect(note.classes()).not.toContain("field-err");

    await wrapper.get('input[value="sqlite"]').trigger("change");
    expect(wrapper.find('details[data-section="paste-url"]').exists()).toBe(false);
    wrapper.unmount();
    replacePaletteConnections([]);
  });

  it("编辑已保存连接时粘贴不同引擎的连接串只填字段不换引擎", async () => {
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api: pageApi(() => ({ status: "ok", stages: [] })) } });
    await flushPromises();
    await wrapper.get("[data-action=conn-menu]").trigger("click");
    await wrapper.get("[data-action=conn-edit]").trigger("click");

    await wrapper.get("[data-field=url-paste]").setValue("postgres://pg_user@pg.example.test:5432/analytics");
    await wrapper.get("[data-action=cf-parse-url]").trigger("click");

    expect((wrapper.get('input[value="mysql"]').element as HTMLInputElement).checked).toBe(true);
    expect((wrapper.get("[data-field=host]").element as HTMLInputElement).value).toBe("pg.example.test");
    expect((wrapper.get("[data-field=port]").element as HTMLInputElement).value).toBe("5432");
    expect((wrapper.get("[data-field=username]").element as HTMLInputElement).value).toBe("pg_user");
    expect(wrapper.get(".paste-note").text()).toContain("连接串是 PostgreSQL 引擎，与该连接不同，仅填充主机等字段");
    wrapper.unmount();
  });

  it("粘贴无法识别的连接串时显示中文原因", async () => {
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api: pageApi(() => ({ status: "ok", stages: [] })) } });
    await flushPromises();
    await wrapper.get("[data-action=conn-new]").trigger("click");

    await wrapper.get("[data-field=url-paste]").setValue("sqlite:C:/data/notes.db");
    await wrapper.get("[data-action=cf-parse-url]").trigger("click");

    const note = wrapper.get(".paste-note");
    expect(note.classes()).toContain("field-err");
    expect(note.text()).toContain("只支持 postgres://、postgresql://、mysql:// 或 mariadb:// 开头的连接串");
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

  it("shows save failures inside the edit dialog and connects on double click", async () => {
    const open = vi.fn(async () => ({ report: { status: "ok" as const, stages: [] }, connected: true, foreignKeys: null }));
    const api: ConnectionApi = {
      ...pageApi(() => ({ status: "ok", stages: [] })),
      save: async () => {
        throw new Error("连接名称已存在");
      },
      open,
    };
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api } });
    await flushPromises();
    await wrapper.get("[data-action=conn-new]").trigger("click");
    await wrapper.get("[data-field=host]").setValue("db.example.test");
    await wrapper.get("[data-field=username]").setValue("reader");
    await wrapper.get("[data-action=cf-save]").trigger("click");
    await flushPromises();
    // 错误必须出现在对话框里，而不是被遮罩挡在页面标题下
    expect(wrapper.get("[data-dialog-error]").text()).toContain("连接名称已存在");
    expect(wrapper.find("[data-action=cf-save]").exists()).toBe(true);

    await wrapper.get(".modal-head .icon-btn").trigger("click");
    await wrapper.get("article.conn-card").trigger("dblclick");
    await flushPromises();
    expect(open).toHaveBeenCalledWith("atlas", expect.objectContaining({ password: "" }));
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

  it("只保留一枚新建连接，并用应用内下拉筛选仍按行显示连接", async () => {
    resetCatalog();
    const wrapper = mount(ConnectionsPage, {
      props: { sqlitePrompt: 0, api: pageApi(() => ({ status: "ok", stages: [] }), [savedMysql(), savedSqlite()]) },
    });
    await flushPromises();

    const createButtons = wrapper.findAll("button").filter((button) => button.text().includes("新建连接"));
    expect(createButtons).toHaveLength(1);
    expect(createButtons[0].classes()).toContain("primary");
    const sqliteButton = wrapper.findAll("button").find((button) => button.text().includes("打开 SQLite"));
    expect(sqliteButton?.classes()).not.toContain("primary");
    expect(wrapper.find(".conn-card.add").exists()).toBe(false);
    expect(wrapper.find(".connection-filters select").exists()).toBe(false);
    expect(wrapper.findAll('.connection-filters [role="combobox"]').map((item) => item.attributes("aria-label"))).toEqual([
      "按数据库类型筛选",
      "按环境筛选",
    ]);

    const mysql = wrapper.findAll("article.conn-card").find((card) => card.text().includes("Atlas"));
    expect(mysql?.text()).toContain("MySQL");
    expect(mysql?.text()).toContain("db.example.test:3306");
    expect(mysql?.text()).toContain("开发");
    expect(mysql?.text()).toContain("未连接");
    expect(mysql?.get("[data-action=conn-connect]").text()).toContain("连接");

    const file = wrapper.findAll("article.conn-card").find((card) => card.text().includes("本地笔记"));
    expect(file?.text()).toContain("SQLite");
    expect(file?.find(".cc-addr").text()).toBe("C:/data/notes.sqlite");
    expect(file?.text()).toContain("本地");
    expect(file?.text()).toContain("未连接");
    expect(file?.get("[data-action=conn-connect]").text()).toContain("连接");

    await wrapper.get('[aria-label="搜索连接"]').setValue("不存在的名称");
    expect(wrapper.text()).toContain("没有匹配的连接");
    expect(wrapper.text()).toContain("试试其他名称、数据库类型或环境。");
    expect(wrapper.text()).not.toContain("连接你的第一个数据库");

    await wrapper.get('[aria-label="搜索连接"]').setValue("");
    await wrapper.get('[aria-label="按环境筛选"]').trigger("click");
    await wrapper.findAll(".listbox-option").find((option) => option.text() === "生产")!.trigger("click");
    expect(wrapper.find("article.conn-card").exists()).toBe(false);
    expect(wrapper.text()).toContain("没有匹配的连接");

    await wrapper.get('[aria-label="按环境筛选"]').trigger("click");
    await wrapper.findAll(".listbox-option").find((option) => option.text() === "全部环境")!.trigger("click");
    await wrapper.get('[aria-label="按数据库类型筛选"]').trigger("click");
    await wrapper.findAll(".listbox-option").find((option) => option.text() === "PostgreSQL")!.trigger("click");
    expect(wrapper.find("article.conn-card").exists()).toBe(false);
    expect(wrapper.find(".connection-filters select").exists()).toBe(false);
    wrapper.unmount();
    resetCatalog();
  });

  it("没有连接时使用和筛选无命中不同的文案", async () => {
    const wrapper = mount(ConnectionsPage, {
      props: { sqlitePrompt: 0, api: pageApi(() => ({ status: "ok", stages: [] }), []) },
    });
    await flushPromises();
    expect(wrapper.text()).toContain("连接你的第一个数据库");
    expect(wrapper.text()).toContain("支持 PostgreSQL、MySQL、MariaDB 和 SQLite。");
    expect(wrapper.text()).not.toContain("没有匹配的连接");
    expect(wrapper.findAll("button").filter((button) => button.text().includes("新建连接"))).toHaveLength(1);
    wrapper.unmount();
  });

  it("已连接的行显示打开和已连接", async () => {
    const connected: CatalogConnection = {
      id: "atlas",
      name: "Atlas",
      engine: "mysql",
      env: "dev",
      envLabel: "开发",
      envTone: "dev",
      status: "connected",
      defaultDatabase: "app",
      foreignKeys: null,
      error: null,
      databases: [],
    };
    replaceCatalog([connected]);
    const wrapper = mount(ConnectionsPage, { props: { sqlitePrompt: 0, api: pageApi(() => ({ status: "ok", stages: [] })) } });
    await flushPromises();
    const card = wrapper.get("article.conn-card");
    expect(card.text()).toContain("已连接");
    expect(card.text()).toContain("MySQL");
    expect(card.text()).toContain("Atlas");
    expect(card.find(".cc-addr").text()).toBe("db.example.test:3306");
    expect(card.text()).toContain("开发");
    const open = card.findAll("footer button").find((button) => button.text().includes("打开"));
    expect(open).toBeTruthy();
    expect(open?.classes()).not.toContain("primary");
    expect(card.find("[data-action=conn-connect]").exists()).toBe(false);
    wrapper.unmount();
    resetCatalog();
  });
});
