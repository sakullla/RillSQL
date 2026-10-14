import { invoke } from "@tauri-apps/api/core";
import { tauriConnectionApi, type OpenOutcome } from "../connections/api";
import type { SavedConnection } from "../connections/model";
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

function invokeFocus(connectionId: string, database: string): Promise<void> {
  return invoke("connections_focus_database", { id: connectionId, database });
}

let focusDatabaseImpl = invokeFocus;

export function setFocusDatabase(focus: ((connectionId: string, database: string) => Promise<void>) | null): void {
  focusDatabaseImpl = focus ?? invokeFocus;
}

export function focusDatabase(connectionId: string, database: string): Promise<void> {
  return focusDatabaseImpl(connectionId, database);
}

function invokeOpen(id: string, password: string): Promise<OpenOutcome> {
  return tauriConnectionApi.open(id, { password, sshPassword: "", passphrase: "" });
}

let openConnectionImpl = invokeOpen;

export function setOpenConnection(open: ((id: string, password: string) => Promise<OpenOutcome>) | null): void {
  openConnectionImpl = open ?? invokeOpen;
}

export function openConnection(id: string, password = ""): Promise<OpenOutcome> {
  return openConnectionImpl(id, password);
}

let listConnectionsImpl = (): Promise<SavedConnection[]> => tauriConnectionApi.list();

export function setListConnections(list: (() => Promise<SavedConnection[]>) | null): void {
  listConnectionsImpl = list ?? (() => tauriConnectionApi.list());
}

export function listConnections(): Promise<SavedConnection[]> {
  return listConnectionsImpl();
}
