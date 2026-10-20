import { invoke } from "@tauri-apps/api/core";

export interface StatementDto {
  sql: string;
  status: string;
  message: string;
  columns: string[];
  columnTypes?: string[];
  rows: (string | null)[][];
  truncated: boolean;
  affected: number | null;
}

export interface RunDto {
  id: string;
  connectionId: string;
  connectionName: string;
  database: string;
  engine: string;
  status: string;
  cancel: string;
  cancelMessage: string;
  viaSsh: boolean;
  freshChannel: boolean;
  statements: StatementDto[];
  startedAt: string;
  elapsedMs: number;
}

export interface StartRequest {
  connectionId: string;
  connectionName: string;
  database: string;
  engine: string;
  sql: string;
  limit: number;
  /** 语句超时（毫秒）。缺省表示不限制。 */
  timeoutMs?: number;
}

export function queryStart(request: StartRequest): Promise<RunDto> {
  return invoke<RunDto>("query_start", { request });
}

export function queryPoll(runId: string, continueRead = false): Promise<RunDto> {
  const args = continueRead ? { runId, continueRead: true } : { runId };
  return invoke<RunDto>("query_poll", args);
}

export function queryCancel(runId: string): Promise<RunDto> {
  return invoke<RunDto>("query_cancel", { runId });
}
