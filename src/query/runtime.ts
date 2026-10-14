import { reactive } from "vue";
import { queryCancel, queryPoll, queryStart, type RunDto, type StartRequest } from "./api";

export interface RunSnapshot extends RunDto {
  tabId: string;
  pollError: string;
}

const runs = reactive<Record<string, RunSnapshot>>({});
const timers = new Map<string, ReturnType<typeof setInterval>>();
const closeAfter = new Set<string>();
let closer: ((id: string) => void) | null = null;
let epoch = 0;

export function tabRun(tabId: string): RunSnapshot | undefined {
  return runs[tabId];
}

export function attachCloser(fn: (id: string) => void) {
  closer = fn;
}

export function connectionBusy(connectionId: string, exceptTabId: string): boolean {
  return Object.entries(runs).some(([tabId, run]) => tabId !== exceptTabId && run.connectionId === connectionId && isLive(run));
}

export async function startTabRun(input: StartRequest & { tabId: string }): Promise<void> {
  if (connectionBusy(input.connectionId, input.tabId)) {
    throw new Error("这个连接已有正在执行的查询。可以切换到其他标签继续编辑。");
  }
  const token = epoch;
  const { tabId, ...request } = input;
  const dto = await queryStart(request);
  if (token !== epoch) return;
  adoptRun(tabId, dto);
  watchRun(tabId);
}

export async function requestCancel(tabId: string): Promise<void> {
  const current = runs[tabId];
  if (!current || !isLive(current)) return;
  if (current.status === "cancel-requested" && current.cancel === "requested") return;
  current.status = "cancel-requested";
  current.cancel = "requested";
  if (!current.cancelMessage) {
    current.cancelMessage = current.viaSsh ? "已请求取消，将另开一条通道。" : "已请求取消，等待数据库确认。";
  }
  const token = epoch;
  const runId = current.id;
  try {
    const next = await queryCancel(runId);
    if (token !== epoch) return;
    apply(tabId, next);
  } catch (err) {
    if (token !== epoch) return;
    const run = runs[tabId];
    if (!run) return;
    run.cancel = "failed";
    run.status = "running";
    run.cancelMessage = errorText(err);
  }
  settle(tabId);
}

export function guardClose(tabId: string): boolean {
  const run = runs[tabId];
  if (!run || !isLive(run)) return true;
  closeAfter.add(tabId);
  void requestCancel(tabId);
  return false;
}

export function adoptRun(tabId: string, dto: RunDto) {
  runs[tabId] = { ...dto, tabId, pollError: "" };
}

export function resetQueryRuntime() {
  epoch += 1;
  for (const timer of timers.values()) clearInterval(timer);
  timers.clear();
  closeAfter.clear();
  closer = null;
  for (const id of Object.keys(runs)) delete runs[id];
}

function watchRun(tabId: string) {
  stopTimer(tabId);
  const timer = setInterval(() => {
    void refresh(tabId);
  }, 400);
  timers.set(tabId, timer);
}

async function refresh(tabId: string) {
  const current = runs[tabId];
  if (!current) {
    stopTimer(tabId);
    return;
  }
  const token = epoch;
  const runId = current.id;
  try {
    const next = await queryPoll(runId);
    if (token !== epoch) return;
    apply(tabId, next);
  } catch (err) {
    if (token !== epoch) return;
    const run = runs[tabId];
    if (run) run.pollError = errorText(err);
  }
  settle(tabId);
}

function apply(tabId: string, next: RunDto) {
  const current = runs[tabId];
  if (current && current.id === next.id && cancelRank(next.cancel) < cancelRank(current.cancel) && isLive(next)) {
    runs[tabId] = {
      ...next,
      tabId,
      pollError: "",
      cancel: current.cancel,
      cancelMessage: current.cancelMessage,
      status: current.status,
    };
    return;
  }
  runs[tabId] = { ...next, tabId, pollError: "" };
}

function settle(tabId: string) {
  const run = runs[tabId];
  if (!run) return;
  if (!isLive(run)) stopTimer(tabId);
  if (!closeAfter.has(tabId)) return;
  if (run.cancel === "confirmed" || run.status === "cancelled") {
    closeAfter.delete(tabId);
    stopTimer(tabId);
    closer?.(tabId);
  }
}

function stopTimer(tabId: string) {
  const timer = timers.get(tabId);
  if (!timer) return;
  clearInterval(timer);
  timers.delete(tabId);
}

function isLive(run: { status: string }): boolean {
  return run.status === "running" || run.status === "cancel-requested";
}

function cancelRank(cancel: string): number {
  if (cancel === "confirmed") return 3;
  if (cancel === "failed") return 2;
  if (cancel === "requested") return 1;
  return 0;
}

export function errorText(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return "执行失败";
}
