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
