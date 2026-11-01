import { executeStructure, previewStructure, type StructureRequest } from "../structure/api";
import type { StructureAction, StructurePlan, StructureReceipt } from "../structure/types";
import type { DatabaseAction } from "./types";

/**
 * 数据库级动作与结构编辑走同一条 ddl 预览/执行通道（R5：写操作先预览再确认）。
 * Rust 侧 Action 枚举已经包含这些变体，前端 structure 的动作联合按任务范围没有扩展，
 * 这里只做类型桥接，不额外发命令、不改执行顺序。
 */
function asStructureAction(action: DatabaseAction): StructureAction {
  return action as StructureAction;
}

export function previewDatabaseAction(request: StructureRequest, action: DatabaseAction): Promise<StructurePlan> {
  return previewStructure(request, asStructureAction(action));
}

export function executeDatabaseAction(
  request: StructureRequest,
  action: DatabaseAction,
  confirmedSql: string[],
): Promise<StructureReceipt> {
  return executeStructure(request, asStructureAction(action), confirmedSql);
}
