import { describe, expect, it } from "vitest";
import { shellMenus } from "./menus";
import { SHORTCUTS } from "./shortcuts";

describe("shellMenus", () => {
  it("exposes the seven desktop menus in order", () => {
    expect(shellMenus({ hasQueryTab: true }, "system").map((menu) => menu.id)).toEqual([
      "file", "edit", "view", "query", "tools", "window", "help",
    ]);
  });

  it("disables every query action when no query tab is active", () => {
    const query = shellMenus({ hasQueryTab: false }, "system").find((menu) => menu.id === "query")!;
    for (const item of query.items) {
      if (item.kind !== "item") continue;
      expect(item.disabled, item.id).toBe(true);
    }
    const enabled = shellMenus({ hasQueryTab: true }, "system").find((menu) => menu.id === "query")!;
    expect(enabled.items.every((item) => item.kind !== "item" || item.disabled !== true)).toBe(true);
  });

  it("marks exactly one theme item as checked for each theme", () => {
    for (const theme of ["light", "dark", "system"] as const) {
      const view = shellMenus({ hasQueryTab: true }, theme).find((menu) => menu.id === "view")!;
      const checked = view.items
        .filter((item): item is Extract<(typeof view.items)[number], { kind: "item" }> => item.kind === "item" && item.checked === true)
        .map((item) => item.id);
      expect(checked).toEqual([`theme-${theme}`]);
    }
  });

  it("shows the editor chords on the query menu", () => {
    const query = shellMenus({ hasQueryTab: true }, "system").find((menu) => menu.id === "query")!;
    const byId = Object.fromEntries(
      query.items.filter((item): item is Extract<(typeof query.items)[number], { kind: "item" }> => item.kind === "item").map((item) => [item.id, item.shortcut]),
    );
    expect(byId.run).toBe("Mod+Enter");
    expect(byId["run-statement"]).toBe("Mod+Shift+Enter");
    expect(byId.cancel).toBe("Mod+.");
    expect(byId.format).toBe("Mod+Shift+F");
    expect(byId.plan).toBe("Mod+Shift+E");
  });

  it("never invents a chord that is absent from the shortcut table", () => {
    const known = new Set(SHORTCUTS.map((item) => item.chord));
    for (const menu of shellMenus({ hasQueryTab: true }, "system")) {
      for (const item of menu.items) {
        if (item.kind !== "item" || !item.shortcut) continue;
        expect(known.has(item.shortcut), `${menu.id}/${item.id} → ${item.shortcut}`).toBe(true);
      }
    }
  });

  it("uses separators only between groups, never at the edges", () => {
    for (const menu of shellMenus({ hasQueryTab: true }, "system")) {
      const first = menu.items[0];
      const last = menu.items[menu.items.length - 1];
      expect(first?.kind, `${menu.id} first`).not.toBe("sep");
      expect(last?.kind, `${menu.id} last`).not.toBe("sep");
      for (let i = 1; i < menu.items.length; i += 1) {
        const prev = menu.items[i - 1];
        const cur = menu.items[i];
        expect(!(prev.kind === "sep" && cur.kind === "sep"), `${menu.id} @${i}`).toBe(true);
      }
    }
  });
});
