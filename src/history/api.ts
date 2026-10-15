import { invoke } from "@tauri-apps/api/core";
import type { SavedDraft, SavedQuery, SavedWrite } from "./saved";
import type { HistoryEntry } from "./plan";

export function queryHistory(): Promise<HistoryEntry[]> {
  return invoke<HistoryEntry[]>("query_history");
}

export function queryHistoryClear(): Promise<void> {
  return invoke<void>("query_history_clear");
}

export function querySavedList(): Promise<SavedQuery[]> {
  return invoke<SavedQuery[]>("query_saved_list");
}

export function querySavedSave(draft: SavedDraft & { name: string }): Promise<SavedWrite> {
  return invoke<SavedWrite>("query_saved_save", { request: draft });
}

export function querySavedRename(id: string, name: string): Promise<SavedQuery> {
  return invoke<SavedQuery>("query_saved_rename", { request: { id, name } });
}

export function querySavedDelete(id: string): Promise<void> {
  return invoke<void>("query_saved_delete", { id });
}
