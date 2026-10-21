import { invoke } from "@tauri-apps/api/core";
import type { Diagram } from "./types";

export function fetchDiagram(connectionId: string, database: string, schema: string): Promise<Diagram> {
  return invoke<Diagram>("diagram_read", { connectionId, database, schema });
}
