import { executeStructure, previewStructure, type StructureRequest } from "../structure/api";
import type { StructureAction, StructurePlan, StructureReceipt } from "../structure/types";
import type { DatabaseAction } from "./types";

/**
 * 数据库级动作与结构编辑走同一条 ddl 预览/执行通道（R5：写操作先预览再确认）。
 * structure 的动作联合已包含这些变体（与 Rust 侧 Action 同名同字段），
 * 这里只收窄类型，不额外发命令、不改执行顺序。
 */
export function previewDatabaseAction(request: StructureRequest, action: DatabaseAction): Promise<StructurePlan> {
  const shared: StructureAction = action;
  return previewStructure(request, shared);
}

export function executeDatabaseAction(
  request: StructureRequest,
  action: DatabaseAction,
  confirmedSql: string[],
): Promise<StructureReceipt> {
  const shared: StructureAction = action;
  return executeStructure(request, shared, confirmedSql);
}
