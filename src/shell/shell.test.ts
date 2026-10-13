import { mount } from "@vue/test-utils";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { filterPalette, shellCommands } from "./commands";
import EnvPill from "./EnvPill.vue";
import ActivityRail from "./ActivityRail.vue";
import { PRODUCT_TITLE } from "./product";
import StatusBar from "./StatusBar.vue";
import { CONNECTION_STATUS_TEXT, ENVIRONMENT_LABELS } from "./status-text";
import TitleBar from "./TitleBar.vue";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

describe("shell layout", () => {
  it("keeps navigation labels, title, and status text", () => {
    const rail = mount(ActivityRail, { props: { page: "workspace" } });
    expect(rail.text()).toContain("工作台");
    expect(rail.text()).toContain("连接");
    expect(rail.text()).toContain("查询历史");
    expect(rail.text()).toContain("设置");

    const title = mount(TitleBar, { props: { theme: "system" } });
    expect(title.text()).toContain("RillSQL");
    expect(title.text()).toContain("澄库");
    expect(title.find(".cmd-trigger").attributes("aria-label")).toBe("打开命令面板");

    const status = mount(StatusBar, {
      props: { tab: { id: "q", kind: "query", title: "查询 1", targetText: null, path: null, draft: "" }, onWorkspace: true },
    });
    expect(status.text()).toContain("未绑定连接 · 请选择执行目标");
    expect(status.text()).toContain("仅存本机");

    const idle = mount(StatusBar, { props: { tab: null, onWorkspace: false } });
    expect(idle.text()).toContain(PRODUCT_TITLE);
  });

  it("does not distinguish environment or status by color alone", () => {
    const pill = mount(EnvPill, { props: { env: "prod", label: ENVIRONMENT_LABELS.prod } });
    expect(pill.text()).toBe("生产");
    expect(pill.classes()).toContain("env-prod");
    for (const label of Object.values(CONNECTION_STATUS_TEXT)) {
      expect(label.length).toBeGreaterThan(1);
    }
  });

  it("filters the command palette down to commands when the query starts with >", () => {
    const items = shellCommands({
      openQuery: () => undefined,
      openStructure: () => undefined,
      showPage: () => undefined,
      toggleSidebar: () => undefined,
      setTheme: () => undefined,
      openHelp: () => undefined,
      requestSqlite: () => undefined,
    });
    const commands = filterPalette(items, ">主题", "all");
    expect(commands.map((item) => item.label)).toEqual(["切换到浅色主题", "切换到深色主题", "主题跟随系统"]);
    expect(filterPalette(items, "customers", "tables")).toEqual([]);
  });

  it("hides navigation labels at the prototype narrow breakpoint", () => {
    const css = readFileSync(resolve(root, "src/styles/shell.css"), "utf8");
    const narrow = css.slice(css.indexOf("@media (max-width: 720px)"));
    expect(narrow).toContain(".rail-btn span { display: none; }");
    expect(css).toContain(".titlebar");
    expect(css).toContain(".statusbar");
    expect(css).toContain("#palette");
  });

  it("puts the product title on the window and the html document", () => {
    const html = readFileSync(resolve(root, "index.html"), "utf8");
    const tauri = readFileSync(resolve(root, "src-tauri/tauri.conf.json"), "utf8");
    expect(html).toContain(`<title>${PRODUCT_TITLE}</title>`);
    expect(tauri).toContain(`"title": "${PRODUCT_TITLE}"`);
    expect(tauri).toContain(`"productName": "${PRODUCT_TITLE}"`);
  });
});
