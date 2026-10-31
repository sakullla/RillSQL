import { invoke } from "@tauri-apps/api/core";
import { tr } from "../i18n";

/** 库摘要：名称 + 集合数（不含 admin/config/local）。 */
export interface MongoDatabaseSummary {
  name: string;
  collections: number;
}

/** 集合摘要：名称 + estimatedDocumentCount 计数（null = 估算不可用）。 */
export interface MongoCollectionSummary {
  name: string;
  estimatedDocuments: number | null;
}

/** 一页文档：idJson 是 _id 的扩展 JSON 文本，json 是整档 relaxed JSON。 */
export interface MongoDoc {
  idJson: string;
  json: string;
}

export interface MongoDocumentsPage {
  total: number;
  documents: MongoDoc[];
}

export interface MongoApi {
  listDatabases(connectionId: string): Promise<MongoDatabaseSummary[]>;
  listCollections(connectionId: string, database: string): Promise<MongoCollectionSummary[]>;
  findDocuments(
    connectionId: string,
    database: string,
    collection: string,
    skip: number,
    limit: number,
    filterJson: string | null,
  ): Promise<MongoDocumentsPage>;
  readDocument(connectionId: string, database: string, collection: string, idJson: string): Promise<{ json: string }>;
  replaceDocument(
    connectionId: string,
    database: string,
    collection: string,
    idJson: string,
    json: string,
  ): Promise<{ matched: number; modified: number }>;
  insertDocument(connectionId: string, database: string, collection: string, json: string): Promise<{ insertedIdJson: string }>;
  deleteDocument(
    connectionId: string,
    database: string,
    collection: string,
    idJson: string,
    confirmed: boolean,
  ): Promise<{ deleted: number }>;
}

function failureText(error: unknown): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message) return error.message;
  if (error && typeof error === "object" && "message" in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message.trim()) return message;
  }
  return tr("mongo.docs.apiFailedUnknown");
}

export function failureOf(error: unknown): string {
  return failureText(error);
}

export const tauriMongoApi: MongoApi = {
  listDatabases: (connectionId) => invoke<MongoDatabaseSummary[]>("mongo_list_databases", { connectionId }),
  listCollections: (connectionId, database) => invoke<MongoCollectionSummary[]>("mongo_list_collections", { connectionId, database }),
  findDocuments: (connectionId, database, collection, skip, limit, filterJson) =>
    invoke<MongoDocumentsPage>("mongo_find_documents", { connectionId, database, collection, skip, limit, filterJson }),
  readDocument: (connectionId, database, collection, idJson) =>
    invoke<{ json: string }>("mongo_read_document", { connectionId, database, collection, idJson }),
  replaceDocument: (connectionId, database, collection, idJson, json) =>
    invoke<{ matched: number; modified: number }>("mongo_replace_document", { connectionId, database, collection, idJson, json }),
  insertDocument: (connectionId, database, collection, json) =>
    invoke<{ insertedIdJson: string }>("mongo_insert_document", { connectionId, database, collection, json }),
  deleteDocument: (connectionId, database, collection, idJson, confirmed) =>
    invoke<{ deleted: number }>("mongo_delete_document", { connectionId, database, collection, idJson, confirmed }),
};
