import { tabHasPending } from "./close-guard";
import { SESSION_KEY, snapshotSession, type ShellTab } from "./session";
import { tr } from "../i18n";

/** A failed checkpoint must block installation: Windows exits without invoking the close handler. */
export function checkpointForUpdate(tabs: readonly ShellTab[], activeId: string | null, busy: boolean, storage: Pick<Storage, "setItem">): void {
  if (busy) throw new Error(tr("shell.update.busy"));
  if (tabs.some((tab) => tabHasPending(tab.id))) throw new Error(tr("shell.update.pending"));
  try {
    storage.setItem(SESSION_KEY, JSON.stringify(snapshotSession(tabs, activeId)));
  } catch {
    throw new Error(tr("shell.update.checkpointFailed"));
  }
}
