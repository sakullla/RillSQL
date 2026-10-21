import type { ConnStatus, Engine } from "../catalog/types";
import type { QueryDraftInput } from "../shell/session";

export interface ScriptTarget {
  connectionId: string | null;
  connectionName: string | null;
  database: string | null;
  engine: Engine | null;
  status: ConnStatus | null;
}

/** 只把脚本放进新查询。不调用执行。 */
export function placeCompareScript(openQueryDraft: (input: QueryDraftInput) => string, sql: string, target: ScriptTarget): string {
  const ready = Boolean(target.connectionId && target.connectionName && target.database != null && target.engine && target.status);
  return openQueryDraft({
    sql,
    connectionId: ready ? target.connectionId : null,
    connectionName: ready ? target.connectionName : null,
    database: ready ? target.database : null,
    engine: ready ? target.engine : null,
    status: ready ? target.status : null,
    needsTarget: !ready,
    notice: null,
  });
}
