import { afterEach, expect, it, vi } from "vitest";
import { checkpointForUpdate } from "./update-session";
import { createSession, SESSION_KEY, restoreSession } from "./session";
import { evaluateWindowClose, registerPending, registerWindowBlocker, resetCloseGuard } from "./close-guard";

afterEach(resetCloseGuard);

it("persists the current SQL draft and active tab before restarting", () => {
  const session = createSession();
  session.openQuery();
  session.activeTab.value!.draft = "SELECT 'unsaved draft'";
  const setItem = vi.fn();
  checkpointForUpdate(session.tabs.value, session.activeId.value, false, { setItem });
  expect(setItem.mock.calls[0]![0]).toBe(SESSION_KEY);
  const saved = restoreSession(setItem.mock.calls[0]![1]);
  expect(saved?.tabs[0]?.draft).toBe("SELECT 'unsaved draft'");
  expect(saved?.activeId).toBe(session.activeId.value);
});

it("blocks active tasks, pending edits, and storage failures", () => {
  const session = createSession();
  session.openQuery();
  const storage = { setItem: vi.fn() };
  expect(() => checkpointForUpdate(session.tabs.value, session.activeId.value, true, storage)).toThrow();
  const release = registerPending(session.activeId.value!, () => true);
  expect(() => checkpointForUpdate(session.tabs.value, session.activeId.value, false, storage)).toThrow();
  expect(storage.setItem).not.toHaveBeenCalled();
  release();
  storage.setItem.mockImplementation(() => { throw new Error("quota"); });
  expect(() => checkpointForUpdate(session.tabs.value, session.activeId.value, false, storage)).toThrow("无法保存");
});

it("blocks native window close while installation is replacing the application", async () => {
  const release = registerWindowBlocker(() => true);
  expect(await evaluateWindowClose()).toBe(false);
  release();
  expect(await evaluateWindowClose()).toBe(true);
});
