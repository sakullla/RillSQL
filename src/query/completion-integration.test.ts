import { closeCompletion, currentCompletions, startCompletion } from "@codemirror/autocomplete";
import { EditorView } from "@codemirror/view";
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import { catalogFixture } from "../catalog/fixtures";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import { clearColumnCache } from "./columns";
import QueryTab from "./QueryTab.vue";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
const unmounts: (() => void)[] = [];

afterEach(() => {
  unmounts.splice(0).forEach((unmount) => unmount());
  resetCatalog();
  clearColumnCache();
  invoke.mockReset();
});

async function editor(draft: string) {
  replaceCatalog(catalogFixture());
  const wrapper = mount(QueryTab, { props: {
    tabId: "completion-test", draft, connectionId: "atlas", connectionName: "Atlas",
    database: "atlas_dev", engine: "postgres", connectionStatus: "connected", notice: null,
  } });
  unmounts.push(() => wrapper.unmount());
  await nextTick();
  const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
  view.dispatch({ selection: { anchor: draft.length } });
  startCompletion(view);
  return { wrapper, view };
}

it.each([
  "SELECT * FROM orders",
  "SELECT * FROM orders o JOIN ord",
  "USE atlas",
])("does not describe columns for table/database completion: %s", async (draft) => {
  const { view } = await editor(draft);
  await vi.waitFor(() => expect(currentCompletions(view.state).length).toBeGreaterThan(0));
  expect(invoke).not.toHaveBeenCalled();
});

it.each([true, false])("reuses JOIN metadata and recomputes aliases (foreign keys: %s)", async (withForeignKeys) => {
  invoke.mockImplementation(async (command: string, args: { request: { table: string; includeForeignKeys: boolean } }) => {
    expect(command).toBe("grid_describe");
    const table = args.request.table;
    return {
      engine: "postgres", schema: "public", name: table, kind: "table", key: ["id"],
      columns: (table === "orders" ? ["id", "customer_id"] : ["id", "name"]).map((name) => ({ name })),
      foreignKeys: withForeignKeys && table === "orders" ? [{
        name: "fk_customer", columns: ["customer_id"], refSchema: "public", refTable: "customers", refColumns: ["id"],
      }] : [],
    };
  });
  const { view } = await editor("SELECT * FROM public.orders o JOIN public.customers c ON ");
  await vi.waitFor(() => expect(currentCompletions(view.state).some((item) => item.label === "o.customer_id = c.id")).toBe(true));
  expect(invoke).toHaveBeenCalledTimes(4);
  closeCompletion(view);
  const next = "SELECT * FROM public.orders ord JOIN public.customers cust ON ";
  view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: next }, selection: { anchor: next.length } });
  startCompletion(view);
  await vi.waitFor(() => expect(currentCompletions(view.state).some((item) => item.label === "ord.customer_id = cust.id")).toBe(true));
  expect(invoke).toHaveBeenCalledTimes(4);
});

it("discards a slow field error after the user edits the statement", async () => {
  let reject!: (reason: unknown) => void;
  invoke.mockImplementation(() => new Promise((_resolve, rejectRequest) => { reject = rejectRequest; }));
  const { wrapper, view } = await editor("SELECT orders.");
  await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
  closeCompletion(view);
  view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: "SELECT 1" }, selection: { anchor: 8 } });
  reject(new Error("outdated metadata error"));
  await flushPromises();
  expect(wrapper.text()).not.toContain("outdated metadata error");
});
