export const CONNECTION_STATUS_TEXT = {
  connected: "已连接",
  disconnected: "未连接",
  connecting: "正在连接",
  error: "连接失败",
  lost: "连接已断开",
} as const;

export type ConnectionStatus = keyof typeof CONNECTION_STATUS_TEXT;

export const ENVIRONMENT_LABELS = {
  dev: "开发",
  test: "测试",
  prod: "生产",
  local: "本地",
} as const;

/** 进行中操作的四态（R4）：状态用文字表达，颜色只作辅助。 */
export const OPERATION_STATE_TEXT = {
  running: "进行中",
  ok: "成功",
  error: "失败",
  unknown: "结果未知",
} as const;

export type OperationState = keyof typeof OPERATION_STATE_TEXT;

export interface OperationActivity {
  state: OperationState;
  text: string;
}
