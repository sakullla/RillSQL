import { invoke } from "@tauri-apps/api/core";
import type { HistoryEntry } from "./plan";

export function queryHistory(): Promise<HistoryEntry[]> {
  return invoke<HistoryEntry[]>("query_history");
}

export function queryHistoryClear(): Promise<void> {
  return invoke<void>("query_history_clear");
}
