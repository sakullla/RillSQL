import { invoke } from "@tauri-apps/api/core";
import type { CompareSide, DataReport, StructureReport } from "./types";

export interface StructureRequest {
  id: string;
  baseline: CompareSide;
  target: CompareSide;
}

export interface DataTableRequest {
  name: string;
  key?: string | null;
}

export function compareStructure(request: StructureRequest): Promise<StructureReport> {
  return invoke<StructureReport>("compare_structure", { request });
}

export function compareData(request: StructureRequest & { tables: DataTableRequest[] }): Promise<DataReport> {
  return invoke<DataReport>("compare_data", { request });
}

export function compareCancel(id: string): Promise<void> {
  return invoke("compare_cancel", { id });
}

export function compareSchemas(connectionId: string, database: string): Promise<string[]> {
  return invoke<string[]>("compare_schemas", { request: { connectionId, database } });
}
