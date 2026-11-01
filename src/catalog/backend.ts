import { invoke } from "@tauri-apps/api/core";
import { tauriConnectionApi, type OpenOutcome } from "../connections/api";
import type { EnvTag, SavedConnection } from "../connections/model";
import type { CatalogObjectRef } from "./object-ref";
import type { CatalogConnection, CatalogExtras, PageRequest, PageResult } from "./types";

export function disconnectConnection(id: string): Promise<void> {
  return tauriConnectionApi.disconnect(id);
}

export interface ObjectLimit {
  connectionId: string;
  database: string;
  schema: string;
  limit: number;
}

/** 轮询返回的会话状态（R4）：connected / lost / reconnecting。 */
export interface SessionStatusInfo {
  id: string;
  status: "connected" | "lost" | "reconnecting";
  reason: string | null;
}

function isSessionStatusInfo(value: unknown): value is SessionStatusInfo {
  if (typeof value !== "object" || value === null) return false;
  const item = value as Record<string, unknown>;
  return (
    typeof item.id === "string" &&
    (item.status === "connected" || item.status === "lost" || item.status === "reconnecting") &&
    (item.reason === null || typeof item.reason === "string")
  );
}

export async function fetchSessions(): Promise<SessionStatusInfo[] | null> {
  try {
    const sessions = await invoke<unknown>("catalog_sessions");
    // 调用方会把结果交给 Map。非数组或形状不对不能变成未处理拒绝。
    if (!Array.isArray(sessions)) return null;
    return sessions.every(isSessionStatusInfo) ? (sessions as SessionStatusInfo[]) : null;
  } catch {
    return null;
  }
}

export async function fetchCatalog(limits: ObjectLimit[], page: number, connectionId?: string | null, outline = false): Promise<CatalogConnection[]> {
  return invoke<CatalogConnection[]>("catalog_refresh", {
    limits,
    page,
    connectionId: connectionId ?? null,
    outline,
  });
}

/** 对象详情读取范围：一个连接上具体某个库/schema（R5）。 */
export interface ExtrasRequest {
  connectionId: string;
  database: string;
  schema: string;
}

/**
 * 按需读取一个连接的对象详情：扩展与序列、事件与分区、PRAGMA 与附加库。
 * 每一节自带失败原因，读不到的节为空，不影响已经加载的树。
 */
export async function fetchExtras(scope: ExtrasRequest): Promise<CatalogExtras | null> {
  const catalogs = await invoke<CatalogConnection[]>("catalog_refresh", {
    limits: [],
    page: 0,
    connectionId: scope.connectionId,
    // 只要对象详情：走轻量 outline，不为了 extras 重读整棵树的对象。
    outline: true,
    extras: {
      connectionId: scope.connectionId,
      database: scope.database,
      schema: scope.schema,
    },
  });
  if (!Array.isArray(catalogs)) return null;
  return catalogs.find((item) => item.id === scope.connectionId)?.extras ?? null;
}

export async function listEnvTags(): Promise<EnvTag[]> {
  return tauriConnectionApi.listTags();
}

export async function fetchDefinition(object: Pick<CatalogObjectRef, "connectionId" | "database" | "schema" | "name" | "kind" | "signature" | "relation">): Promise<string> {
  const sql = await invoke<string>("catalog_definition", {
    connectionId: object.connectionId,
    database: object.database,
    schema: object.schema,
    name: object.name,
    kind: object.kind,
    signature: object.signature ?? "",
    relation: object.relation ?? "",
  });
  if (!sql.trim()) throw new Error(tr("catalog.definition.failed"));
  return sql;
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

import { tr } from "../i18n";