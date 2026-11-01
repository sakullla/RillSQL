import { reactive } from "vue";

export type ToastTone = "ok" | "err";

export interface ToastItem {
  id: number;
  text: string;
  tone: ToastTone;
}

const VISIBLE_MS = 2400;
const MAX_TOASTS = 3;

let nextId = 1;

export const toasts = reactive<ToastItem[]>([]);

function dismiss(id: number) {
  const index = toasts.findIndex((item) => item.id === id);
  if (index >= 0) toasts.splice(index, 1);
}

/** 轻量操作回执（已复制、已断开）：自动消失，错误仍由原位提示承担。 */
export function toast(text: string, tone: ToastTone = "ok"): void {
  const id = nextId++;
  toasts.push({ id, text, tone });
  while (toasts.length > MAX_TOASTS) toasts.shift();
  setTimeout(() => dismiss(id), VISIBLE_MS);
}
