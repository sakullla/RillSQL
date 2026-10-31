import { tr } from "../i18n";

/** 连接状态枚举：状态字走字典 shell.status.*（原来是常量表，i18n 后改为函数按当前语言取值）。 */
export type ConnectionStatus = "connected" | "disconnected" | "connecting" | "error" | "lost";

const CONNECTION_STATUS_KEYS: Record<ConnectionStatus, string> = {
  connected: "shell.status.connected",
  disconnected: "shell.status.disconnected",
  connecting: "shell.status.connecting",
  error: "shell.status.error",
  lost: "shell.status.lost",
};

export function isConnectionStatus(status: unknown): status is ConnectionStatus {
  return typeof status === "string" && status in CONNECTION_STATUS_KEYS;
}

export function connectionStatusText(status: ConnectionStatus): string {
  return tr(CONNECTION_STATUS_KEYS[status]);
}

export type EnvironmentTag = "dev" | "test" | "prod" | "local";

const ENVIRONMENT_KEYS: Record<EnvironmentTag, string> = {
  dev: "shell.environment.dev",
  test: "shell.environment.test",
  prod: "shell.environment.prod",
  local: "shell.environment.local",
};

export function environmentLabel(tag: EnvironmentTag): string {
  return tr(ENVIRONMENT_KEYS[tag]);
}

/** 进行中操作的四态（R4）：状态用文字表达，颜色只作辅助。 */
export type OperationState = "running" | "ok" | "error" | "unknown";

const OPERATION_STATE_KEYS: Record<OperationState, string> = {
  running: "shell.status.running",
  ok: "shell.status.ok",
  error: "shell.status.failed",
  unknown: "shell.status.unknown",
};

export function operationStateText(state: OperationState): string {
  return tr(OPERATION_STATE_KEYS[state]);
}

export interface OperationActivity {
  state: OperationState;
  text: string;
}
