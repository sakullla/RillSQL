import { invoke } from "@tauri-apps/api/core";
import type { TableTarget } from "../grid/target";
import type { StructureAction, StructureDescription, StructurePlan, StructureReceipt } from "./types";

export interface StructureRequest {
  connectionId: string;
  database: string;
  schema: string;
  engine: string;
}

export function describeStructure(target: TableTarget): Promise<StructureDescription> {
  return invoke<StructureDescription>("ddl_describe", {
    request: {
      connectionId: target.connectionId,
      database: target.database,
      schema: target.schema,
      table: target.table,
      engine: target.engine,
    },
  });
}

export function previewStructure(request: StructureRequest, action: StructureAction): Promise<StructurePlan> {
  return invoke<StructurePlan>("ddl_preview", { request: { ...request, action } });
}

export function executeStructure(request: StructureRequest, action: StructureAction, confirmedSql: string[]): Promise<StructureReceipt> {
  return invoke<StructureReceipt>("ddl_execute", { request: { ...request, action, confirmedSql } });
}
