import { invoke } from "@tauri-apps/api/core";
import type { CatalogConnection, PageRequest, PageResult } from "./types";

export interface ObjectLimit {
  connectionId: string;
  database: string;
  schema: string;
  limit: number;
}

export async function fetchSessions(): Promise<string[] | null> {
  try {
    return await invoke<string[]>("catalog_sessions");
  } catch {
    return null;
  }
}

export async function fetchCatalog(limits: ObjectLimit[], page: number): Promise<CatalogConnection[]> {
  return invoke<CatalogConnection[]>("catalog_refresh", { limits, page });
}

export async function fetchPage(request: PageRequest): Promise<PageResult> {
  return invoke<PageResult>("catalog_page", {
    connectionId: request.connectionId,
    database: request.database,
    schema: request.schema,
    offset: request.offset,
    limit: request.limit,
  });
}
